use std::borrow::Cow;

use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Default)]
pub(super) struct TableState {
    pub(super) rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_cell: bool,
    current_link_destination: Option<String>,
}

impl TableState {
    pub(super) fn start_row(&mut self) {
        self.current_row.clear();
    }

    pub(super) fn start_cell(&mut self) {
        self.current_cell.clear();
        self.in_cell = true;
    }

    pub(super) fn push_text(&mut self, text: &str) {
        if self.in_cell {
            self.current_cell.push_str(text);
        }
    }

    pub(super) fn push_html(&mut self, html: &str) {
        let trimmed = html.trim();
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "<br>" | "<br/>" | "<br />"
        ) {
            self.push_text("\n");
        } else {
            self.push_text(html);
        }
    }

    pub(super) fn start_link(&mut self, destination: String) {
        self.current_link_destination = Some(destination);
    }

    pub(super) fn end_link(&mut self) {
        let Some(destination) = self.current_link_destination.take() else {
            return;
        };
        if self.in_cell && !destination.is_empty() {
            self.current_cell.push_str(" (");
            self.current_cell.push_str(&destination);
            self.current_cell.push(')');
        }
    }

    pub(super) fn end_cell(&mut self) {
        self.current_link_destination = None;
        self.current_row
            .push(std::mem::take(&mut self.current_cell));
        self.in_cell = false;
    }

    pub(super) fn end_row(&mut self) {
        if !self.current_row.is_empty() {
            self.rows.push(std::mem::take(&mut self.current_row));
        }
    }
}

#[derive(Debug)]
struct TableLayoutCandidate {
    column_widths: Vec<usize>,
    padding: usize,
    hard_wrap: bool,
}

#[derive(Debug)]
struct TableMetrics {
    average_body_row_height: f64,
    max_body_row_height: usize,
    hard_wraps_url_or_code: bool,
    hard_wrap_count: usize,
}

pub(super) fn render_table_lines(rows: &[Vec<String>], width: Option<usize>) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }

    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let available_width = width.unwrap_or(usize::MAX / 4).max(1);
    let normalized_rows = normalize_table_rows(rows, column_count);
    let widths = desired_column_widths(&normalized_rows, column_count);

    match choose_table_layout(&normalized_rows, &widths, available_width, column_count) {
        Some(candidate) => render_box_table(
            &normalized_rows,
            &candidate.column_widths,
            candidate.padding,
            candidate.hard_wrap,
        )
        .into_iter()
        .map(Line::from)
        .collect(),
        None => render_vertical_table(&normalized_rows, available_width),
    }
}

pub(super) fn normalize_table_boundaries(input: &str) -> Cow<'_, str> {
    if !input.contains('|') {
        return Cow::Borrowed(input);
    }

    let lines = input.split_inclusive('\n').collect::<Vec<_>>();
    let mut out = String::with_capacity(input.len());
    let mut changed = false;
    let mut index = 0;
    let mut code_fence: Option<(char, usize)> = None;
    while index < lines.len() {
        if let Some(fence) = code_fence {
            out.push_str(lines[index]);
            if is_closing_code_fence(lines[index], fence) {
                code_fence = None;
            }
            index += 1;
        } else if let Some(fence) = opening_code_fence(lines[index]) {
            code_fence = Some(fence);
            out.push_str(lines[index]);
            index += 1;
        } else if is_indented_code_line(lines[index]) {
            out.push_str(lines[index]);
            index += 1;
        } else if index + 1 < lines.len()
            && is_table_row_source(lines[index])
            && is_table_delimiter_source(lines[index + 1])
        {
            out.push_str(lines[index]);
            out.push_str(lines[index + 1]);
            index += 2;

            while index < lines.len() && is_table_row_source(lines[index]) {
                out.push_str(lines[index]);
                index += 1;
            }

            if index < lines.len() && !lines[index].trim().is_empty() {
                out.push('\n');
                changed = true;
            }
        } else {
            out.push_str(lines[index]);
            index += 1;
        }
    }

    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(input)
    }
}

fn opening_code_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = strip_fence_indent(line)?;
    let mut chars = trimmed.chars();
    let marker = chars.next()?;
    if marker != '`' && marker != '~' {
        return None;
    }

    let marker_count = 1 + chars.take_while(|ch| *ch == marker).count();
    (marker_count >= 3).then_some((marker, marker_count))
}

fn is_closing_code_fence(line: &str, (marker, opening_count): (char, usize)) -> bool {
    let Some(trimmed) = strip_fence_indent(line) else {
        return false;
    };
    let marker_count = trimmed.chars().take_while(|ch| *ch == marker).count();
    marker_count >= opening_count
        && trimmed[marker.len_utf8() * marker_count..]
            .trim()
            .is_empty()
}

fn strip_fence_indent(line: &str) -> Option<&str> {
    let mut spaces = 0usize;
    for (index, ch) in line.char_indices() {
        if ch != ' ' {
            return (spaces <= 3).then_some(&line[index..]);
        }
        spaces += 1;
        if spaces > 3 {
            return None;
        }
    }
    Some("")
}

fn is_indented_code_line(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

fn is_table_row_source(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.contains('|')
}

fn is_table_delimiter_source(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|').trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.split('|').all(|cell| {
        let cell = cell.trim();
        let dash_count = cell.chars().filter(|ch| *ch == '-').count();
        dash_count >= 3 && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
    })
}

fn normalize_table_rows(rows: &[Vec<String>], column_count: usize) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| {
            let mut normalized = row.clone();
            normalized.resize(column_count, String::new());
            normalized
        })
        .collect()
}

fn desired_column_widths(rows: &[Vec<String>], column_count: usize) -> Vec<usize> {
    let mut widths = vec![3; column_count];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.width());
        }
    }
    widths
}

fn choose_table_layout(
    rows: &[Vec<String>],
    desired_widths: &[usize],
    available_width: usize,
    column_count: usize,
) -> Option<TableLayoutCandidate> {
    if available_width < 32 || width_shape_requires_vertical(column_count, available_width) {
        return None;
    }

    let normal = allocate_table_widths(
        desired_widths,
        available_width,
        column_count,
        /*padding*/ 1,
    )
    .and_then(|widths| {
        build_table_candidate(
            rows,
            desired_widths,
            widths,
            available_width,
            /*padding*/ 1,
        )
    });
    if normal.is_some() {
        return normal;
    }

    allocate_table_widths(
        desired_widths,
        available_width,
        column_count,
        /*padding*/ 0,
    )
    .and_then(|widths| {
        build_table_candidate(
            rows,
            desired_widths,
            widths,
            available_width,
            /*padding*/ 0,
        )
    })
}

fn width_shape_requires_vertical(column_count: usize, available_width: usize) -> bool {
    (column_count >= 5 && available_width < 72) || (column_count >= 6 && available_width < 96)
}

fn build_table_candidate(
    rows: &[Vec<String>],
    desired_widths: &[usize],
    column_widths: Vec<usize>,
    available_width: usize,
    padding: usize,
) -> Option<TableLayoutCandidate> {
    let metrics = table_metrics(rows, &column_widths, /*hard_wrap*/ false);
    let needs_hard_wrap = metrics.hard_wrap_count > 0 || metrics.max_body_row_height > 12;
    let (hard_wrap, metrics) = if needs_hard_wrap {
        (
            true,
            table_metrics(rows, &column_widths, /*hard_wrap*/ true),
        )
    } else {
        (false, metrics)
    };

    if should_render_vertical(
        rows,
        desired_widths,
        &column_widths,
        available_width,
        padding,
        hard_wrap,
        &metrics,
    ) {
        return None;
    }

    Some(TableLayoutCandidate {
        column_widths,
        padding,
        hard_wrap,
    })
}

fn allocate_table_widths(
    desired_widths: &[usize],
    available_width: usize,
    column_count: usize,
    padding: usize,
) -> Option<Vec<usize>> {
    let border_width = column_count + 1;
    let padding_width = padding * 2 * column_count;
    let available_content_width = available_width.checked_sub(border_width + padding_width)?;
    let min_total = 3 * column_count;
    if available_content_width < min_total {
        return None;
    }

    let mut widths = vec![3; column_count];
    let mut remaining = available_content_width - min_total;
    while remaining > 0 {
        let mut changed = false;
        for index in 0..column_count {
            if remaining == 0 {
                break;
            }
            if widths[index] < desired_widths[index] {
                widths[index] += 1;
                remaining -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    Some(widths)
}

fn render_box_table(
    rows: &[Vec<String>],
    column_widths: &[usize],
    padding: usize,
    hard_wrap: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    out.push(border_line("┌", "┬", "┐", column_widths, padding));

    for (index, row) in rows.iter().enumerate() {
        out.extend(render_table_row(row, column_widths, padding, hard_wrap));
        if index == 0 {
            out.push(border_line("├", "┼", "┤", column_widths, padding));
        }
    }

    out.push(border_line("└", "┴", "┘", column_widths, padding));
    out
}

fn render_table_row(
    row: &[String],
    column_widths: &[usize],
    padding: usize,
    hard_wrap: bool,
) -> Vec<String> {
    let wrapped_cells = row
        .iter()
        .zip(column_widths)
        .map(|(cell, width)| wrap_table_cell(cell, *width, hard_wrap))
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1);
    let mut out = Vec::with_capacity(row_height);

    for line_index in 0..row_height {
        let mut line = String::from("│");
        for (cell_lines, width) in wrapped_cells.iter().zip(column_widths) {
            let content = cell_lines.get(line_index).map(String::as_str).unwrap_or("");
            line.push_str(&" ".repeat(padding));
            line.push_str(content);
            line.push_str(&" ".repeat(width.saturating_sub(content.width())));
            line.push_str(&" ".repeat(padding));
            line.push('│');
        }
        out.push(line);
    }

    out
}

fn border_line(
    left: &str,
    separator: &str,
    right: &str,
    column_widths: &[usize],
    padding: usize,
) -> String {
    let cell_segments = column_widths
        .iter()
        .map(|width| "─".repeat(width + padding * 2))
        .collect::<Vec<_>>();
    format!("{left}{}{right}", cell_segments.join(separator))
}

fn wrap_table_cell(cell: &str, width: usize, hard_wrap: bool) -> Vec<String> {
    if cell.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let options = textwrap::Options::new(width)
        .break_words(hard_wrap)
        .word_separator(textwrap::WordSeparator::AsciiSpace)
        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);

    for segment in cell.split('\n') {
        let wrapped = textwrap::wrap(segment, options.clone())
            .into_iter()
            .map(std::borrow::Cow::into_owned)
            .collect::<Vec<_>>();
        if wrapped.is_empty() {
            lines.push(String::new());
        } else {
            lines.extend(wrapped);
        }
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn table_metrics(rows: &[Vec<String>], column_widths: &[usize], hard_wrap: bool) -> TableMetrics {
    let mut row_heights = Vec::with_capacity(rows.len());
    let mut hard_wraps_url_or_code = false;
    let mut hard_wrap_count = 0usize;

    for row in rows {
        let row_height = row
            .iter()
            .zip(column_widths)
            .map(|(cell, width)| {
                if cell_needs_hard_wrap(cell, *width) {
                    hard_wrap_count += 1;
                    hard_wraps_url_or_code |= is_url_or_code_like(cell);
                }
                wrap_table_cell(cell, *width, hard_wrap).len()
            })
            .max()
            .unwrap_or(1);
        row_heights.push(row_height);
    }

    let body_heights = row_heights.iter().skip(1).copied().collect::<Vec<_>>();
    let body_row_count = body_heights.len();
    let max_body_row_height = body_heights.iter().copied().max().unwrap_or(0);
    let average_body_row_height = if body_row_count == 0 {
        0.0
    } else {
        body_heights.iter().sum::<usize>() as f64 / body_row_count as f64
    };

    TableMetrics {
        average_body_row_height,
        max_body_row_height,
        hard_wraps_url_or_code,
        hard_wrap_count,
    }
}

fn cell_needs_hard_wrap(cell: &str, width: usize) -> bool {
    cell.split('\n')
        .flat_map(str::split_whitespace)
        .any(|token| token.width() > width)
}

fn should_render_vertical(
    rows: &[Vec<String>],
    desired_widths: &[usize],
    column_widths: &[usize],
    available_width: usize,
    padding: usize,
    hard_wrap: bool,
    metrics: &TableMetrics,
) -> bool {
    let column_count = column_widths.len();
    let body_rows = rows.len().saturating_sub(1);
    if metrics.max_body_row_height > 12
        || (body_rows >= 10 && metrics.average_body_row_height > 2.5)
        || (body_rows >= 24 && metrics.average_body_row_height > 1.75)
    {
        return true;
    }

    if hard_wrap && ((body_rows > 5 && metrics.hard_wraps_url_or_code) || body_rows >= 10) {
        return true;
    }

    if column_count >= 4 && (body_rows >= 8 || column_count >= 5) {
        let starved_columns = column_widths.iter().filter(|width| **width <= 3).count();
        if starved_columns > 1 {
            return true;
        }

        for (index, width) in column_widths.iter().enumerate().take(column_count) {
            if is_index_column(rows, index) {
                continue;
            }
            if is_content_heavy_column(rows, index) && *width < 12 {
                return true;
            }
        }
    }

    let slack = available_width.saturating_sub(table_total_width(column_widths, padding));
    has_width_risk_chars(rows)
        && column_count >= 3
        && available_width <= 48
        && slack < 2
        && desired_widths.iter().sum::<usize>() + column_count + 1 >= available_width
}

fn table_total_width(column_widths: &[usize], padding: usize) -> usize {
    column_widths.iter().sum::<usize>()
        + column_widths.len()
        + 1
        + padding * 2 * column_widths.len()
}

fn is_index_column(rows: &[Vec<String>], index: usize) -> bool {
    let header = rows
        .first()
        .and_then(|row| row.get(index))
        .map(|header| normalized_header(header))
        .unwrap_or_default();
    if matches!(header.as_str(), "#" | "id" | "idx" | "index" | "row") {
        return true;
    }

    rows.iter()
        .skip(1)
        .filter_map(|row| row.get(index))
        .filter(|cell| !cell.trim().is_empty())
        .all(|cell| cell.trim().chars().all(|ch| ch.is_ascii_digit()))
}

fn is_content_heavy_column(rows: &[Vec<String>], index: usize) -> bool {
    let header = rows
        .first()
        .and_then(|row| row.get(index))
        .map(|header| normalized_header(header))
        .unwrap_or_default();
    if [
        "link",
        "url",
        "code",
        "sample",
        "content",
        "description",
        "summary",
        "expectation",
        "notes",
    ]
    .iter()
    .any(|needle| header.contains(needle))
    {
        return true;
    }

    rows.iter()
        .skip(1)
        .filter_map(|row| row.get(index))
        .any(|cell| is_url_or_code_like(cell) || cell.width() > 24)
}

fn is_url_or_code_like(cell: &str) -> bool {
    cell.contains("://")
        || cell.contains("::")
        || cell.contains("=>")
        || cell.contains("->")
        || cell.contains('`')
        || cell.contains('{')
        || cell.contains('}')
        || cell.contains('(')
        || cell.contains(')')
}

fn has_width_risk_chars(rows: &[Vec<String>]) -> bool {
    rows.iter().flatten().any(|cell| {
        cell.chars().any(|ch| {
            matches!(ch, '\u{fe0f}' | '\u{200d}') || ('\u{1f300}'..='\u{1faff}').contains(&ch)
        })
    })
}

fn normalized_header(header: &str) -> String {
    header.trim().to_ascii_lowercase()
}

fn render_vertical_table(rows: &[Vec<String>], available_width: usize) -> Vec<Line<'static>> {
    let Some((headers, body_rows)) = rows.split_first() else {
        return Vec::new();
    };
    let included_columns = included_vertical_columns(headers, body_rows);
    if included_columns.is_empty() {
        return Vec::new();
    }

    let max_header_width = included_columns
        .iter()
        .map(|index| vertical_label(headers, *index).width())
        .max()
        .unwrap_or(4);
    let label_width = max_header_width.min(20).min(available_width / 3).max(4);
    let value_width = available_width.saturating_sub(label_width + 2).max(1);
    let mut out = Vec::new();

    for (row_index, row) in body_rows.iter().enumerate() {
        if row_index > 0 {
            out.push(Line::default());
        }
        out.push(
            Line::from(format_vertical_row_title(headers, row, row_index))
                .dim()
                .bold(),
        );
        for index in &included_columns {
            let label = truncate_to_width(&vertical_label(headers, *index), label_width);
            let cell = row.get(*index).map(String::as_str).unwrap_or("").trim();
            let value = if cell.is_empty() { "—" } else { cell };
            let wrapped = textwrap::wrap(
                value,
                textwrap::Options::new(value_width)
                    .break_words(false)
                    .word_separator(textwrap::WordSeparator::AsciiSpace)
                    .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
            )
            .into_iter()
            .map(std::borrow::Cow::into_owned)
            .collect::<Vec<_>>();
            let wrapped = if wrapped.is_empty() {
                vec![String::new()]
            } else {
                wrapped
            };

            for (line_index, value_line) in wrapped.iter().enumerate() {
                if line_index == 0 {
                    let prefix = format!("{label:>label_width$}: ");
                    let value_span = if cell.is_empty() {
                        Span::from(value_line.clone()).dim()
                    } else {
                        Span::from(value_line.clone())
                    };
                    out.push(Line::from(vec![prefix.dim(), value_span]));
                } else {
                    out.push(Line::from(vec![
                        " ".repeat(label_width + 2).into(),
                        value_line.clone().into(),
                    ]));
                }
            }
        }
    }
    out
}

fn included_vertical_columns(headers: &[String], body_rows: &[Vec<String>]) -> Vec<usize> {
    let first_column_titles_rows = headers
        .first()
        .map(|header| normalized_header(header))
        .is_some_and(|header| matches!(header.as_str(), "#" | "id" | "idx" | "index" | "row"))
        && headers.len() > 1;

    (0..headers.len())
        .filter(|index| !(first_column_titles_rows && *index == 0))
        .filter(|index| {
            !headers[*index].trim().is_empty()
                || body_rows
                    .iter()
                    .any(|row| row.get(*index).is_some_and(|cell| !cell.trim().is_empty()))
        })
        .collect()
}

fn vertical_label(headers: &[String], index: usize) -> String {
    headers
        .get(index)
        .map(|header| header.trim())
        .filter(|header| !header.is_empty())
        .unwrap_or("Column")
        .to_string()
}

fn format_vertical_row_title(headers: &[String], row: &[String], row_index: usize) -> String {
    let first_header = headers.first().map(|header| normalized_header(header));
    let first_cell = row.first().map(|cell| cell.trim()).unwrap_or("");
    if first_header
        .as_deref()
        .is_some_and(|header| matches!(header, "#" | "id" | "idx" | "index" | "row"))
        && !first_cell.is_empty()
    {
        format!("Row {first_cell}")
    } else {
        format!("Row {}", row_index + 1)
    }
}

fn truncate_to_width(input: &str, max_width: usize) -> String {
    if input.width() <= max_width {
        return input.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let target = max_width - 1;
    let mut width = 0usize;
    for ch in input.chars() {
        let ch_width = ch.to_string().width();
        if width + ch_width > target {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}
