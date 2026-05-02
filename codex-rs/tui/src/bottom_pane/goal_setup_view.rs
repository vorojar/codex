use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;
use std::cell::RefCell;

use codex_app_server_protocol::ThreadGoalBudgetParams;

use crate::render::renderable::Renderable;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::bottom_pane_view::ViewCompletion;
use super::popup_consts::standard_popup_hint_line;
use super::textarea::TextArea;
use super::textarea::TextAreaState;

pub(crate) type GoalSetupSubmitted =
    Box<dyn Fn(String, Option<ThreadGoalBudgetParams>) + Send + Sync>;

pub(crate) struct GoalSetupView {
    objective: TextArea,
    objective_state: RefCell<TextAreaState>,
    budget_value: TextArea,
    budget_value_state: RefCell<TextAreaState>,
    budget_kind: GoalBudgetKind,
    focus: GoalSetupFocus,
    error: Option<String>,
    on_submit: GoalSetupSubmitted,
    completion: Option<ViewCompletion>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GoalBudgetKind {
    None,
    Tokens,
    FiveHourLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GoalSetupFocus {
    Objective,
    Budget,
    Value,
}

impl GoalSetupView {
    pub(crate) fn new(on_submit: GoalSetupSubmitted) -> Self {
        Self {
            objective: TextArea::new(),
            objective_state: RefCell::new(TextAreaState::default()),
            budget_value: TextArea::new(),
            budget_value_state: RefCell::new(TextAreaState::default()),
            budget_kind: GoalBudgetKind::None,
            focus: GoalSetupFocus::Objective,
            error: None,
            on_submit,
            completion: None,
        }
    }

    fn submit(&mut self) {
        let objective = self.objective.text().trim().to_string();
        if objective.is_empty() {
            self.error = Some("Goal objective must not be empty.".to_string());
            return;
        }
        match self.budget() {
            Ok(budget) => {
                (self.on_submit)(objective, budget);
                self.completion = Some(ViewCompletion::Accepted);
            }
            Err(err) => self.error = Some(err),
        }
    }

    fn budget(&self) -> Result<Option<ThreadGoalBudgetParams>, String> {
        let value = self.budget_value.text().trim();
        match self.budget_kind {
            GoalBudgetKind::None => Ok(None),
            GoalBudgetKind::Tokens => Ok(Some(ThreadGoalBudgetParams::Tokens {
                token_budget: parse_token_budget(value)?,
            })),
            GoalBudgetKind::FiveHourLimit => {
                Ok(Some(ThreadGoalBudgetParams::FiveHourLimitPercent {
                    percent: parse_five_hour_percent(value)?,
                }))
            }
        }
    }

    fn cycle_budget(&mut self) {
        self.budget_kind = match self.budget_kind {
            GoalBudgetKind::None => GoalBudgetKind::Tokens,
            GoalBudgetKind::Tokens => GoalBudgetKind::FiveHourLimit,
            GoalBudgetKind::FiveHourLimit => GoalBudgetKind::None,
        };
        if self.budget_kind == GoalBudgetKind::None && self.focus == GoalSetupFocus::Value {
            self.focus = GoalSetupFocus::Budget;
        }
        self.error = None;
    }

    fn cycle_focus(&mut self) {
        self.focus = match (self.focus, self.budget_kind) {
            (GoalSetupFocus::Objective, _) => GoalSetupFocus::Budget,
            (GoalSetupFocus::Budget, GoalBudgetKind::None) => GoalSetupFocus::Objective,
            (GoalSetupFocus::Budget, _) => GoalSetupFocus::Value,
            (GoalSetupFocus::Value, _) => GoalSetupFocus::Objective,
        };
    }

    fn budget_label(&self) -> &'static str {
        match self.budget_kind {
            GoalBudgetKind::None => "None",
            GoalBudgetKind::Tokens => "Tokens",
            GoalBudgetKind::FiveHourLimit => "5h limit",
        }
    }

    fn value_placeholder(&self) -> &'static str {
        match self.budget_kind {
            GoalBudgetKind::None => "",
            GoalBudgetKind::Tokens => "500000 or 500k",
            GoalBudgetKind::FiveHourLimit => "10%",
        }
    }
}

impl BottomPaneView for GoalSetupView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        self.error = None;
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => self.cycle_focus(),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus == GoalSetupFocus::Budget => self.cycle_budget(),
            KeyEvent {
                code: KeyCode::Left | KeyCode::Right | KeyCode::Char(' '),
                ..
            } if self.focus == GoalSetupFocus::Budget => self.cycle_budget(),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.submit(),
            other => match self.focus {
                GoalSetupFocus::Objective => self.objective.input(other),
                GoalSetupFocus::Value => self.budget_value.input(other),
                GoalSetupFocus::Budget => {}
            },
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.completion = Some(ViewCompletion::Cancelled);
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        match self.focus {
            GoalSetupFocus::Objective => self.objective.insert_str(&pasted),
            GoalSetupFocus::Value => self.budget_value.insert_str(&pasted),
            GoalSetupFocus::Budget => return false,
        }
        true
    }
}

impl Renderable for GoalSetupView {
    fn desired_height(&self, width: u16) -> u16 {
        let value_rows = if self.budget_kind == GoalBudgetKind::None {
            0
        } else {
            2
        };
        6u16.saturating_add(self.objective_height(width))
            .saturating_add(value_rows)
            .saturating_add(u16::from(self.error.is_some()))
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let mut y = area.y;
        Paragraph::new(Line::from(vec![gutter(), "Set goal".bold()])).render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
        y = y.saturating_add(1);

        let budget_style = if self.focus == GoalSetupFocus::Budget {
            self.budget_label().cyan()
        } else {
            self.budget_label().into()
        };
        Paragraph::new(Line::from(vec![
            gutter(),
            "Budget: ".dim(),
            budget_style,
            "  ".into(),
            "tab focus, arrows cycle".dim(),
        ]))
        .render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
        y = y.saturating_add(1);

        if self.budget_kind != GoalBudgetKind::None {
            Paragraph::new(Line::from(vec![gutter(), "Value: ".dim()])).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y = y.saturating_add(1);
            self.render_textarea(
                &self.budget_value,
                &self.budget_value_state,
                self.value_placeholder(),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y = y.saturating_add(1);
        }

        Paragraph::new(Line::from(vec![gutter(), "Objective: ".dim()])).render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
        y = y.saturating_add(1);
        let objective_height = self.objective_height(area.width);
        self.render_textarea(
            &self.objective,
            &self.objective_state,
            "What should Codex keep working toward?",
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: objective_height,
            },
            buf,
        );
        y = y.saturating_add(objective_height);

        if let Some(error) = &self.error {
            Paragraph::new(Line::from(vec![gutter(), error.clone().red()])).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y = y.saturating_add(1);
        }

        let hint_y = y.saturating_add(1);
        if hint_y < area.y.saturating_add(area.height) {
            Paragraph::new(standard_popup_hint_line()).render(
                Rect {
                    x: area.x,
                    y: hint_y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if area.width <= 2 {
            return None;
        }
        match self.focus {
            GoalSetupFocus::Objective => {
                let y = area.y
                    + 3
                    + if self.budget_kind == GoalBudgetKind::None {
                        0
                    } else {
                        2
                    };
                let rect = Rect {
                    x: area.x.saturating_add(2),
                    y,
                    width: area.width.saturating_sub(2),
                    height: self.objective_height(area.width),
                };
                self.objective
                    .cursor_pos_with_state(rect, *self.objective_state.borrow())
            }
            GoalSetupFocus::Value if self.budget_kind != GoalBudgetKind::None => {
                let rect = Rect {
                    x: area.x.saturating_add(2),
                    y: area.y.saturating_add(3),
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                self.budget_value
                    .cursor_pos_with_state(rect, *self.budget_value_state.borrow())
            }
            _ => None,
        }
    }
}

impl GoalSetupView {
    fn objective_height(&self, width: u16) -> u16 {
        self.objective
            .desired_height(width.saturating_sub(2))
            .clamp(2, 6)
    }

    fn render_textarea(
        &self,
        textarea: &TextArea,
        state: &RefCell<TextAreaState>,
        placeholder: &str,
        area: Rect,
        buf: &mut Buffer,
    ) {
        for row in 0..area.height {
            Paragraph::new(Line::from(vec![gutter()])).render(
                Rect {
                    x: area.x,
                    y: area.y.saturating_add(row),
                    width: 2,
                    height: 1,
                },
                buf,
            );
        }
        let textarea_rect = Rect {
            x: area.x.saturating_add(2),
            y: area.y,
            width: area.width.saturating_sub(2),
            height: area.height,
        };
        Clear.render(textarea_rect, buf);
        StatefulWidgetRef::render_ref(&textarea, textarea_rect, buf, &mut *state.borrow_mut());
        if textarea.text().is_empty() {
            Paragraph::new(Line::from(placeholder.to_string().dim())).render(textarea_rect, buf);
        }
    }
}

fn parse_token_budget(value: &str) -> Result<i64, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Token budget must not be empty.".to_string());
    }
    let (number, multiplier) = match value.chars().last() {
        Some('k' | 'K') => (&value[..value.len() - 1], 1_000.0),
        Some('m' | 'M') => (&value[..value.len() - 1], 1_000_000.0),
        _ => (value, 1.0),
    };
    let parsed = number
        .parse::<f64>()
        .map_err(|_| "Token budget must be a number like 500000 or 500k.".to_string())?;
    let budget = (parsed * multiplier).round();
    if !budget.is_finite() || budget <= 0.0 || budget > i64::MAX as f64 {
        return Err("Token budget must be positive.".to_string());
    }
    Ok(budget as i64)
}

fn parse_five_hour_percent(value: &str) -> Result<f64, String> {
    let value = value
        .trim()
        .strip_suffix('%')
        .unwrap_or(value.trim())
        .trim();
    let percent = value
        .parse::<f64>()
        .map_err(|_| "5h limit must be a percent like 10%.".to_string())?;
    if !percent.is_finite() || percent <= 0.0 || percent > 100.0 {
        return Err("5h limit must be between 0% and 100%.".to_string());
    }
    Ok(percent)
}

fn gutter() -> Span<'static> {
    "| ".cyan()
}
