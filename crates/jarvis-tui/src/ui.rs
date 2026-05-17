//! Rendering. Pure function over `AppState` → frame.

use crate::app::{AppState, Focus};
use jarvis_api::{Event, Task};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use tui_input::Input;

pub fn render(f: &mut Frame, state: &AppState, input: &Input) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(root[0]);

    render_tasks(f, main[0], state);
    render_events(f, main[1], state);
    render_status(f, root[1], state);

    match state.focus {
        Focus::NewTask => render_new_task_modal(f, input),
        Focus::ConfirmCancel => render_confirm_modal(f, state),
        Focus::Help => render_help_modal(f),
        Focus::Tasks => {}
    }
}

fn render_tasks(f: &mut Frame, area: Rect, state: &AppState) {
    let items: Vec<ListItem> = state
        .tasks
        .iter()
        .map(|t| ListItem::new(task_line(t)))
        .collect();
    let title = format!(
        " Tasks ({}) [{}] ",
        state.tasks.len(),
        if state.show_all { "all" } else { "active" }
    );
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.tasks.len().saturating_sub(1))));

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(highlight_border(state.focus == Focus::Tasks)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut list_state);
}

fn task_line(t: &Task) -> Line<'_> {
    let status_style = status_style(&t.status);
    let backend = if t.sandbox.is_empty() {
        String::from("-")
    } else if t.sandbox == "native" {
        "native".to_string()
    } else {
        format!("{}/{}", t.sandbox, t.net_policy)
    };
    Line::from(vec![
        Span::styled(format!("{:<8} ", short_id(&t.id)), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:<10}", t.status), status_style),
        Span::raw(" "),
        Span::styled(
            format!("{:<18}", clip(&backend, 18)),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::raw(clip(&t.goal, 80)),
    ])
}

fn render_events(f: &mut Frame, area: Rect, state: &AppState) {
    let title = match state.selected_task() {
        Some(t) => format!(" Events — {} [{}] ", short_id(&t.id), t.status),
        None => " Events (no task selected) ".to_string(),
    };
    let lines: Vec<Line> = state.events.iter().map(event_line).collect();
    let p = Paragraph::new(lines)
        .block(Block::default().title(title).borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn event_line(ev: &Event) -> Line<'_> {
    let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(ev.ts_micros)
        .map(|t| t.format("%H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| ev.ts_micros.to_string());
    let kind_style = match ev.kind.as_str() {
        "verdict" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        "error" => Style::default().fg(Color::Red),
        "tool_call" => Style::default().fg(Color::Yellow),
        "tool_result" => Style::default().fg(Color::LightGreen),
        "decision" => Style::default().fg(Color::Cyan),
        "heartbeat" => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    };
    let summary = pretty_payload(&ev.kind, &ev.payload_json);
    Line::from(vec![
        Span::styled(format!("{ts}  "), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:<13} ", ev.kind), kind_style),
        Span::styled(format!("#{:<4} ", ev.id), Style::default().fg(Color::DarkGray)),
        Span::raw(if ev.subject.is_empty() { String::new() } else { format!("[{}] ", clip(&ev.subject, 32)) }),
        Span::raw(clip(&summary, 600)),
    ])
}

fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    let left = if let Some(err) = &state.error {
        Span::styled(format!("✗ {err}"), Style::default().fg(Color::Red))
    } else if let Some(s) = &state.status {
        Span::styled(format!("✓ {s}"), Style::default().fg(Color::Green))
    } else {
        Span::raw(state.daemon_info.clone())
    };
    let hints = "  [n]ew  [c]ancel  [r]efresh  [a]ll  [j/k] nav  [?] help  [q]uit";
    let line = Line::from(vec![
        left,
        Span::styled(hints, Style::default().fg(Color::DarkGray)),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn render_new_task_modal(f: &mut Frame, input: &Input) {
    let area = centered_rect(60, 8, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(" New task — Enter to submit, Esc to cancel ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let p = Paragraph::new(input.value())
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false });
    f.render_widget(p, inner);
    // Cursor at end of input.
    let cursor_x = inner.x + input.visual_cursor() as u16;
    f.set_cursor_position((cursor_x.min(inner.x + inner.width.saturating_sub(1)), inner.y));
}

fn render_confirm_modal(f: &mut Frame, state: &AppState) {
    let area = centered_rect(50, 5, f.area());
    f.render_widget(Clear, area);
    let id = state
        .selected_task()
        .map(|t| short_id(&t.id))
        .unwrap_or_default();
    let p = Paragraph::new(Line::from(vec![
        Span::raw(format!("Cancel task {id}? "),),
        Span::styled("[y]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Span::raw(" / "),
        Span::styled("[n]", Style::default().fg(Color::Green)),
    ]))
    .block(
        Block::default()
            .title(" Confirm cancel ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red)),
    );
    f.render_widget(p, area);
}

fn render_help_modal(f: &mut Frame) {
    let area = centered_rect(60, 14, f.area());
    f.render_widget(Clear, area);
    let lines = vec![
        Line::from("Navigation"),
        Line::from("  j / ↓        next task"),
        Line::from("  k / ↑        previous task"),
        Line::from("  a            toggle active / all"),
        Line::from("  r            refresh now"),
        Line::from(""),
        Line::from("Actions"),
        Line::from("  n            new task"),
        Line::from("  c            cancel selected task"),
        Line::from("  q / Esc      quit"),
        Line::from(""),
        Line::from("  ?            toggle this help"),
    ];
    let p = Paragraph::new(lines).block(
        Block::default()
            .title(" Help — press any key to dismiss ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(p, area);
}

fn centered_rect(percent_x: u16, lines: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(lines) / 2),
            Constraint::Length(lines),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn status_style(status: &str) -> Style {
    match status {
        "running" => Style::default().fg(Color::Yellow),
        "completed" => Style::default().fg(Color::Green),
        "failed" => Style::default().fg(Color::Red),
        "cancelled" => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

fn highlight_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn short_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

fn pretty_payload(kind: &str, json: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return json.to_string(),
    };
    match kind {
        "decision" => v.get("thought").and_then(|s| s.as_str()).map(String::from)
            .unwrap_or_else(|| json.to_string()),
        "tool_call" => format!(
            "{}({})",
            v.get("tool").and_then(|s| s.as_str()).unwrap_or("?"),
            v.get("args").map(|a| a.to_string()).unwrap_or_default()
        ),
        "tool_result" => v.get("summary").and_then(|s| s.as_str()).map(String::from)
            .unwrap_or_else(|| json.to_string()),
        "verdict" => {
            let verdict = v.get("verdict").and_then(|s| s.as_str()).unwrap_or("?");
            let msg = v.get("message").and_then(|s| s.as_str()).unwrap_or("");
            format!("[{verdict}] {msg}")
        }
        "error" => v
            .get("message")
            .or_else(|| v.get("error"))
            .and_then(|s| s.as_str())
            .map(String::from)
            .unwrap_or_else(|| json.to_string()),
        "heartbeat" => v
            .get("step")
            .map(|s| format!("step {s}"))
            .unwrap_or_default(),
        _ => json.to_string(),
    }
}
