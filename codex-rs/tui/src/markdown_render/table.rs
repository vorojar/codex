use super::table_cell::TableCell;
use std::borrow::Cow;
use std::ops::Range;

use crate::render::line_utils::line_to_static;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_line;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

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
    hard_wrap_count: usize,
}

pub(super) fn render_table_lines(
    rows: &[Vec<TableCell>],
    width: Option<usize>,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }

    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let normalized_rows = normalize_table_rows(rows, column_count);
    let terminal_width = width.unwrap_or(usize::MAX / 4);
    let safety_columns = if has_width_risk_chars(&normalized_rows) {
        2
    } else {
        1
    };
    let available_width = terminal_width.saturating_sub(safety_columns).max(1);
    let widths = desired_column_widths(&normalized_rows, column_count);

    match choose_table_layout(&normalized_rows, &widths, available_width, column_count) {
        Some(candidate) => render_box_table(
            &normalized_rows,
            &candidate.column_widths,
            candidate.padding,
            candidate.hard_wrap,
        ),
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
            push_table_row_source(&mut out, lines[index], &mut changed);
            out.push_str(lines[index + 1]);
            index += 2;

            while index < lines.len() && is_table_row_source(lines[index]) {
                push_table_row_source(&mut out, lines[index], &mut changed);
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
    !trimmed.is_empty() && table_cell_sources(trimmed).len() > 1
}

fn is_table_delimiter_source(line: &str) -> bool {
    let mut cells = table_cell_sources(line.trim());
    if cells.first().is_some_and(|cell| cell.trim().is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.trim().is_empty()) {
        cells.pop();
    }
    if cells.is_empty() {
        return false;
    }
    cells.iter().all(|cell| {
        let cell = cell.trim();
        let dash_count = cell.chars().filter(|ch| *ch == '-').count();
        dash_count >= 3 && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
    })
}

fn push_table_row_source(out: &mut String, line: &str, changed: &mut bool) {
    match escape_inline_code_pipes_in_table_row(line) {
        Cow::Borrowed(line) => out.push_str(line),
        Cow::Owned(line) => {
            *changed = true;
            out.push_str(&line);
        }
    }
}

fn escape_inline_code_pipes_in_table_row(line: &str) -> Cow<'_, str> {
    if !line.contains('`') {
        return Cow::Borrowed(line);
    }

    let code_ranges = inline_code_span_ranges(line);
    if code_ranges.is_empty() {
        return Cow::Borrowed(line);
    }

    let mut out: Option<String> = None;
    let mut last = 0;
    for (index, ch) in line.char_indices() {
        if ch == '|'
            && is_index_in_ranges(index, &code_ranges)
            && !is_backslash_escaped(line.as_bytes(), index)
        {
            let out = out.get_or_insert_with(|| String::with_capacity(line.len() + 1));
            out.push_str(&line[last..index]);
            out.push('\\');
            last = index;
        }
    }

    match out {
        Some(mut out) => {
            out.push_str(&line[last..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(line),
    }
}

fn table_cell_sources(line: &str) -> Vec<&str> {
    let code_ranges = inline_code_span_ranges(line);
    let mut cells = Vec::new();
    let mut cell_start = 0;
    for (index, ch) in line.char_indices() {
        if ch == '|'
            && !is_index_in_ranges(index, &code_ranges)
            && !is_backslash_escaped(line.as_bytes(), index)
        {
            cells.push(&line[cell_start..index]);
            cell_start = index + ch.len_utf8();
        }
    }
    cells.push(&line[cell_start..]);
    cells
}

fn inline_code_span_ranges(line: &str) -> Vec<Range<usize>> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index = next_char_index(line, index + 1).unwrap_or(bytes.len());
            }
            b'`' => {
                let delimiter_len = repeated_ascii_len(bytes, index, /*needle*/ b'`');
                if let Some(closing_start) =
                    find_closing_backtick_run(line, index + delimiter_len, delimiter_len)
                {
                    let closing_end = closing_start + delimiter_len;
                    ranges.push(index..closing_end);
                    index = closing_end;
                } else {
                    index += delimiter_len;
                }
            }
            _ => {
                index = next_char_index(line, index).unwrap_or(bytes.len());
            }
        }
    }
    ranges
}

fn find_closing_backtick_run(line: &str, start: usize, delimiter_len: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let run_len = repeated_ascii_len(bytes, index, /*needle*/ b'`');
            if run_len == delimiter_len {
                return Some(index);
            }
            index += run_len;
        } else {
            index = next_char_index(line, index).unwrap_or(bytes.len());
        }
    }
    None
}

fn repeated_ascii_len(bytes: &[u8], start: usize, needle: u8) -> usize {
    bytes[start..]
        .iter()
        .take_while(|byte| **byte == needle)
        .count()
}

fn next_char_index(line: &str, index: usize) -> Option<usize> {
    line.get(index..)?
        .chars()
        .next()
        .map(|ch| index + ch.len_utf8())
}

fn is_index_in_ranges(index: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|range| range.contains(&index))
}

fn is_backslash_escaped(bytes: &[u8], index: usize) -> bool {
    let mut backslashes = 0;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        backslashes += 1;
        cursor -= 1;
    }
    backslashes % 2 == 1
}

fn normalize_table_rows(rows: &[Vec<TableCell>], column_count: usize) -> Vec<Vec<TableCell>> {
    rows.iter()
        .map(|row| {
            let mut normalized = row.clone();
            normalized.resize(column_count, TableCell::default());
            normalized
        })
        .collect()
}

fn desired_column_widths(rows: &[Vec<TableCell>], column_count: usize) -> Vec<usize> {
    let mut widths = vec![3; column_count];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.width());
        }
    }
    widths
}

fn choose_table_layout(
    rows: &[Vec<TableCell>],
    desired_widths: &[usize],
    available_width: usize,
    column_count: usize,
) -> Option<TableLayoutCandidate> {
    let normal = allocate_table_widths(
        rows,
        desired_widths,
        available_width,
        column_count,
        /*padding*/ 1,
    )
    .and_then(|widths| {
        build_table_candidate(rows, widths, available_width, /*padding*/ 1)
    });
    if normal.is_some() {
        return normal;
    }

    allocate_table_widths(
        rows,
        desired_widths,
        available_width,
        column_count,
        /*padding*/ 0,
    )
    .and_then(|widths| {
        build_table_candidate(rows, widths, available_width, /*padding*/ 0)
    })
}

fn build_table_candidate(
    rows: &[Vec<TableCell>],
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

    if should_render_vertical(rows, &column_widths, available_width, padding, &metrics) {
        return None;
    }

    Some(TableLayoutCandidate {
        column_widths,
        padding,
        hard_wrap,
    })
}

fn allocate_table_widths(
    rows: &[Vec<TableCell>],
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

    let basic_targets = desired_widths
        .iter()
        .enumerate()
        .map(|(index, desired)| {
            let header = rows
                .first()
                .and_then(|row| row.get(index))
                .map(normalized_header)
                .unwrap_or_default();
            let is_icon_column = matches!(header.as_str(), "icon" | "emoji");
            let compact_target = if is_index_column(rows, index) || is_icon_column {
                4
            } else {
                6
            };
            (*desired).min(compact_target).max(3)
        })
        .collect::<Vec<_>>();
    grow_columns_to_targets(&mut widths, &mut remaining, &basic_targets);

    let content_targets = desired_widths
        .iter()
        .enumerate()
        .map(|(index, desired)| {
            if is_content_heavy_column(rows, index) {
                (*desired).min(24).max(widths[index])
            } else {
                widths[index]
            }
        })
        .collect::<Vec<_>>();
    grow_columns_to_targets(&mut widths, &mut remaining, &content_targets);
    grow_columns_to_targets(&mut widths, &mut remaining, desired_widths);

    Some(widths)
}

fn grow_columns_to_targets(widths: &mut [usize], remaining: &mut usize, targets: &[usize]) {
    while *remaining > 0 {
        let mut changed = false;
        for index in 0..widths.len() {
            if *remaining == 0 {
                break;
            }
            if widths[index] < targets[index] {
                widths[index] += 1;
                *remaining -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn render_box_table(
    rows: &[Vec<TableCell>],
    column_widths: &[usize],
    padding: usize,
    hard_wrap: bool,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(border_line(
        "┌",
        "┬",
        "┐",
        column_widths,
        padding,
    )));

    for (index, row) in rows.iter().enumerate() {
        out.extend(render_table_row(row, column_widths, padding, hard_wrap));
        if index == 0 {
            out.push(Line::from(border_line(
                "├",
                "┼",
                "┤",
                column_widths,
                padding,
            )));
        }
    }

    out.push(Line::from(border_line(
        "└",
        "┴",
        "┘",
        column_widths,
        padding,
    )));
    out
}

fn render_table_row(
    row: &[TableCell],
    column_widths: &[usize],
    padding: usize,
    hard_wrap: bool,
) -> Vec<Line<'static>> {
    let wrapped_cells = row
        .iter()
        .zip(column_widths)
        .map(|(cell, width)| wrap_table_cell(cell, *width, hard_wrap))
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1);
    let mut out = Vec::with_capacity(row_height);

    for line_index in 0..row_height {
        let mut spans = vec![Span::from("│")];
        for (cell_lines, width) in wrapped_cells.iter().zip(column_widths) {
            let content = cell_lines.get(line_index);
            push_padding(&mut spans, padding);
            if let Some(content) = content {
                spans.extend(content.spans.iter().cloned());
                push_padding(&mut spans, width.saturating_sub(content.width()));
            } else {
                push_padding(&mut spans, *width);
            }
            push_padding(&mut spans, padding);
            spans.push(Span::from("│"));
        }
        out.push(Line::from(spans));
    }

    out
}

fn push_padding(spans: &mut Vec<Span<'static>>, width: usize) {
    if width > 0 {
        spans.push(Span::from(" ".repeat(width)));
    }
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

fn wrap_table_cell(cell: &TableCell, width: usize, hard_wrap: bool) -> Vec<Line<'static>> {
    if cell.lines().is_empty() {
        return vec![Line::default()];
    }
    let mut lines = Vec::new();
    let options = RtOptions::new(width)
        .break_words(hard_wrap)
        .word_separator(textwrap::WordSeparator::AsciiSpace)
        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);

    for segment in cell.lines() {
        if segment.width() == 0 {
            lines.push(Line::default());
            continue;
        }
        let wrapped = word_wrap_line(segment, options.clone())
            .into_iter()
            .map(|line| line_to_static(&line))
            .collect::<Vec<_>>();
        if wrapped.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrapped);
        }
    }

    if lines.is_empty() {
        vec![Line::default()]
    } else {
        lines
    }
}

fn table_metrics(
    rows: &[Vec<TableCell>],
    column_widths: &[usize],
    hard_wrap: bool,
) -> TableMetrics {
    let mut row_heights = Vec::with_capacity(rows.len());
    let mut hard_wrap_count = 0usize;

    for row in rows {
        let row_height = row
            .iter()
            .zip(column_widths)
            .map(|(cell, width)| {
                if cell_needs_hard_wrap(cell, *width) {
                    hard_wrap_count += 1;
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
        hard_wrap_count,
    }
}

fn cell_needs_hard_wrap(cell: &TableCell, width: usize) -> bool {
    cell.plain_text()
        .split('\n')
        .flat_map(str::split_whitespace)
        .any(|token| token.width() > width)
}

fn should_render_vertical(
    rows: &[Vec<TableCell>],
    column_widths: &[usize],
    available_width: usize,
    padding: usize,
    metrics: &TableMetrics,
) -> bool {
    let column_count = column_widths.len();
    let body_rows = rows.len().saturating_sub(1);
    if metrics.max_body_row_height > 24
        || (body_rows >= 10 && metrics.average_body_row_height > 6.0)
        || (body_rows >= 24 && metrics.average_body_row_height > 4.0)
    {
        return true;
    }

    let non_index_columns = (0..column_count)
        .filter(|index| !is_index_column(rows, *index))
        .count();
    let starved_non_index_columns = column_widths
        .iter()
        .enumerate()
        .filter(|(index, width)| !is_index_column(rows, *index) && **width <= 3)
        .count();
    if column_count >= 4
        && body_rows >= 12
        && non_index_columns > 0
        && starved_non_index_columns == non_index_columns
    {
        return true;
    }

    has_width_risk_chars(rows)
        && available_width <= 24
        && table_total_width(column_widths, padding) >= available_width
}

fn table_total_width(column_widths: &[usize], padding: usize) -> usize {
    column_widths.iter().sum::<usize>()
        + column_widths.len()
        + 1
        + padding * 2 * column_widths.len()
}

fn is_index_column(rows: &[Vec<TableCell>], index: usize) -> bool {
    let header = rows
        .first()
        .and_then(|row| row.get(index))
        .map(normalized_header)
        .unwrap_or_default();
    if matches!(header.as_str(), "#" | "id" | "idx" | "index" | "row") {
        return true;
    }

    rows.iter()
        .skip(1)
        .filter_map(|row| row.get(index))
        .map(TableCell::trimmed_plain_text)
        .filter(|cell| !cell.is_empty())
        .all(|cell| cell.chars().all(|ch| ch.is_ascii_digit()))
}

fn is_content_heavy_column(rows: &[Vec<TableCell>], index: usize) -> bool {
    let header = rows
        .first()
        .and_then(|row| row.get(index))
        .map(normalized_header)
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

fn is_url_or_code_like(cell: &TableCell) -> bool {
    let text = cell.plain_text();
    text.contains("://")
        || text.contains("::")
        || text.contains("=>")
        || text.contains("->")
        || text.contains('`')
        || text.contains('{')
        || text.contains('}')
        || text.contains('(')
        || text.contains(')')
}

fn has_width_risk_chars(rows: &[Vec<TableCell>]) -> bool {
    rows.iter().flatten().any(|cell| {
        cell.plain_text().chars().any(|ch| {
            matches!(ch, '\u{fe0f}' | '\u{200d}') || ('\u{1f300}'..='\u{1faff}').contains(&ch)
        })
    })
}

fn normalized_header(header: &TableCell) -> String {
    header.trimmed_plain_text().to_ascii_lowercase()
}

fn render_vertical_table(rows: &[Vec<TableCell>], available_width: usize) -> Vec<Line<'static>> {
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
    let max_value_width = body_rows
        .iter()
        .flat_map(|row| included_columns.iter().filter_map(|index| row.get(*index)))
        .flat_map(|cell| {
            cell.plain_text()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .map(|line| line.width())
        .max()
        .unwrap_or(1);
    let available_content_width = available_width.saturating_sub(7);
    let (label_width, value_width) = if available_content_width < 6 {
        let label_width = available_content_width.saturating_div(2).max(1);
        (
            label_width,
            available_content_width.saturating_sub(label_width).max(1),
        )
    } else {
        let min_label_width = 3;
        let min_value_width = 3;
        let desired_label_width = max_header_width.min(20).max(min_label_width);
        let label_width =
            desired_label_width.min(available_content_width.saturating_sub(min_value_width));
        let desired_value_width = max_value_width.max(min_value_width);
        let value_width = desired_value_width
            .min(available_content_width.saturating_sub(label_width))
            .max(min_value_width);
        (label_width, value_width)
    };
    let mut out = Vec::new();

    out.push(Line::from(border_line(
        "┌",
        "┬",
        "┐",
        &[label_width, value_width],
        /*padding*/ 1,
    )));
    for (row_index, row) in body_rows.iter().enumerate() {
        if row_index > 0 {
            out.push(Line::from(border_line(
                "├",
                "┼",
                "┤",
                &[label_width, value_width],
                /*padding*/ 1,
            )));
        }
        for index in &included_columns {
            let label = truncate_to_width(&vertical_label(headers, *index), label_width);
            let empty_cell = TableCell::default();
            let cell = row.get(*index).unwrap_or(&empty_cell);
            let wrapped = if cell.is_blank() {
                vec![Line::from("—")]
            } else {
                wrap_table_cell(cell, value_width, /*hard_wrap*/ true)
            };
            let wrapped = if wrapped.is_empty() {
                vec![Line::default()]
            } else {
                wrapped
            };

            for (line_index, value_line) in wrapped.iter().enumerate() {
                let label = if line_index == 0 { label.as_str() } else { "" };
                let label_padding = label_width.saturating_sub(label.width());
                let value_padding = value_width.saturating_sub(value_line.width());
                let mut spans = vec![Span::from("│ ")];
                push_padding(&mut spans, label_padding);
                spans.push(Span::from(label.to_string()));
                spans.push(Span::from(" │ "));
                spans.extend(value_line.spans.iter().cloned());
                push_padding(&mut spans, value_padding);
                spans.push(Span::from(" │"));
                out.push(Line::from(spans));
            }
        }
    }
    out.push(Line::from(border_line(
        "└",
        "┴",
        "┘",
        &[label_width, value_width],
        /*padding*/ 1,
    )));
    out
}

fn included_vertical_columns(headers: &[TableCell], body_rows: &[Vec<TableCell>]) -> Vec<usize> {
    let first_column_titles_rows = headers
        .first()
        .map(normalized_header)
        .is_some_and(|header| matches!(header.as_str(), "#" | "id" | "idx" | "index" | "row"))
        && headers.len() > 1;

    (0..headers.len())
        .filter(|index| !(first_column_titles_rows && *index == 0))
        .filter(|index| {
            !headers[*index].is_blank()
                || body_rows
                    .iter()
                    .any(|row| row.get(*index).is_some_and(|cell| !cell.is_blank()))
        })
        .collect()
}

fn vertical_label(headers: &[TableCell], index: usize) -> String {
    headers
        .get(index)
        .map(TableCell::trimmed_plain_text)
        .filter(|header| !header.is_empty())
        .unwrap_or_else(|| "Column".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn table_cell_sources_ignore_escaped_and_inline_code_pipes() {
        let cells = table_cell_sources("| a | `b | c` | d \\| e |");

        assert_eq!(
            cells.iter().map(|cell| cell.trim()).collect::<Vec<_>>(),
            vec!["", "a", "`b | c`", "d \\| e", ""]
        );
    }

    #[test]
    fn table_row_normalization_escapes_raw_pipes_inside_inline_code() {
        assert_eq!(
            escape_inline_code_pipes_in_table_row("| A | `a | b` | ``x ` | y`` |\n").as_ref(),
            "| A | `a \\| b` | ``x ` \\| y`` |\n"
        );
    }

    #[test]
    fn table_row_normalization_preserves_existing_escaped_pipes() {
        assert_eq!(
            escape_inline_code_pipes_in_table_row("| A | `a \\| b` |\n").as_ref(),
            "| A | `a \\| b` |\n"
        );
    }

    #[test]
    fn table_row_normalization_ignores_unclosed_inline_code() {
        assert_eq!(
            escape_inline_code_pipes_in_table_row("| A | `a | b |\n").as_ref(),
            "| A | `a | b |\n"
        );
    }
}
