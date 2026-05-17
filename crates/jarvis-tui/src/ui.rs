//! Borderless rendering inspired by OpenCode: 80/20 split, soft palette,
//! conversation-style event stream on the left, contextual sidebar on the right,
//! always-visible input bar at the bottom.

use crate::app::{AppState, Focus};
use crate::theme::{status_color, Theme};
use jarvis_api::{Event, Task};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;
use tui_input::Input;

pub fn render(f: &mut Frame, state: &AppState, input: &Input) {
    let t = &state.theme;

    // Vertical: main area / status line / input line.
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Horizontal split inside main: 80% main column, 20% sidebar.
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(76), Constraint::Percentage(24)])
        .split(root[0]);

    render_events(f, main[0], state, t);
    render_sidebar(f, main[1], state, t);
    render_status(f, root[1], state, t);
    render_input(f, root[2], state, input, t);

    match state.focus {
        Focus::ConfirmCancel => render_confirm_modal(f, state, t),
        Focus::Help => render_help_modal(f, t),
        Focus::Normal => {}
    }
}

// ---------- Main column: conversation-style events ----------

fn render_events(f: &mut Frame, area: Rect, state: &AppState, t: &Theme) {
    let block = Block::default().padding(Padding::new(1, 1, 0, 0));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::with_capacity(state.events.len() + 4);

    // Header: current task summary or instructions.
    if let Some(task) = state.selected_task() {
        lines.push(Line::from(vec![
            Span::styled(
                task.goal.clone(),
                Style::default()
                    .fg(t.heading)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            format!(
                "{}  ·  {}  ·  {}",
                task.status,
                if task.sandbox.is_empty() {
                    "-".to_string()
                } else if task.sandbox == "native" {
                    "native".to_string()
                } else {
                    format!("{}/{}", task.sandbox, task.net_policy)
                },
                short_id(&task.id),
            ),
            Style::default().fg(t.dim),
        )]));
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(vec![Span::styled(
            "No task selected",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "Type a goal and press Enter to start.  Press ? for help.",
            Style::default().fg(t.fade),
        )]));
    }

    for ev in state.events.iter() {
        push_event_lines(&mut lines, ev, t, inner.width as usize);
    }

    // Auto-scroll: show the tail that fits.
    let total = lines.len();
    let visible = inner.height as usize;
    let skip = total.saturating_sub(visible);
    let view: Vec<Line> = lines.into_iter().skip(skip).collect();

    let p = Paragraph::new(view).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

fn push_event_lines(out: &mut Vec<Line<'static>>, ev: &Event, t: &Theme, width: usize) {
    let _ = width;
    match ev.kind.as_str() {
        "decision" => {
            let txt = thought_of(&ev.payload_json).unwrap_or_default();
            if !txt.is_empty() {
                out.push(Line::from(vec![Span::styled(
                    txt,
                    Style::default().fg(t.assistant).add_modifier(Modifier::ITALIC),
                )]));
            }
        }
        "tool_call" => {
            let (tool, args_one) = tool_call_summary(&ev.payload_json);
            out.push(Line::from(vec![
                Span::styled("→ ", Style::default().fg(t.fade)),
                Span::styled(tool, Style::default().fg(t.accent)),
                Span::raw(" "),
                Span::styled(args_one, Style::default().fg(t.dim)),
            ]));
        }
        "tool_result" => {
            let (summary, exit, ok) = tool_result_summary(&ev.payload_json);
            let glyph = if ok { "  ✓ " } else { "  ✗ " };
            let style = if ok {
                Style::default().fg(t.good)
            } else {
                Style::default().fg(t.error)
            };
            out.push(Line::from(vec![
                Span::styled(glyph, style),
                Span::styled(summary, Style::default().fg(t.dim)),
                Span::raw(if exit.is_empty() { String::new() } else { format!(" ({exit})") }),
            ]));
        }
        "error" => {
            let msg = error_message(&ev.payload_json);
            out.push(Line::from(vec![
                Span::styled("  ✗ ", Style::default().fg(t.error)),
                Span::styled(msg, Style::default().fg(t.error)),
            ]));
        }
        "verdict" => {
            let (verdict, msg) = verdict_summary(&ev.payload_json);
            let (glyph, color) = match verdict.as_str() {
                "pass" => ("■", t.good),
                "fail" => ("■", t.error),
                _ => ("■", t.warn),
            };
            out.push(Line::from(""));
            out.push(Line::from(vec![
                Span::styled(format!("{glyph} {verdict} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(msg, Style::default().fg(t.body)),
            ]));
        }
        "heartbeat" => {
            // Render only step boundaries to keep the log uncluttered.
            if let Some(step) = step_of(&ev.payload_json) {
                out.push(Line::from(vec![Span::styled(
                    format!("─ step {step} ─"),
                    Style::default().fg(t.fade),
                )]));
            }
        }
        _ => {}
    }
}

// ---------- Right sidebar ----------

fn render_sidebar(f: &mut Frame, area: Rect, state: &AppState, t: &Theme) {
    let block = Block::default().padding(Padding::new(2, 1, 0, 0));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // Section: selected task.
    if let Some(task) = state.selected_task() {
        section(&mut lines, "Task", t);
        lines.push(kv("id", &short_id(&task.id), t.body));
        lines.push(kv("status", &task.status, status_color(&task.status, t)));
        if !task.sandbox.is_empty() {
            section(&mut lines, "Sandbox", t);
            lines.push(kv("backend", &task.sandbox, t.body));
            if !task.net_policy.is_empty() {
                lines.push(kv("network", &task.net_policy, t.body));
            }
            if !task.worktree_path.is_empty() {
                section(&mut lines, "Worktree", t);
                lines.push(kv("branch", &task.worktree_branch, t.body));
            }
        }
        section(&mut lines, "Counters", t);
        let (steps, evcount, kinds) = counters_for(&state.events);
        lines.push(kv("step", &steps, t.body));
        lines.push(kv("events", &evcount.to_string(), t.body));
        for (k, v) in kinds.iter().take(4) {
            lines.push(kv(k, &v.to_string(), t.dim));
        }
    } else {
        section(&mut lines, "Tasks", t);
        lines.push(Line::from(vec![Span::styled(
            "none",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        )]));
    }

    // Section: all tasks (mini list).
    section(&mut lines, "All tasks", t);
    if state.tasks.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            if state.show_all { "no tasks yet" } else { "no active tasks" },
            Style::default().fg(t.dim),
        )]));
    } else {
        for (i, task) in state.tasks.iter().take(10).enumerate() {
            let cursor = if i == state.selected { "▸ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(cursor, Style::default().fg(t.focus)),
                Span::styled(format!("{} ", short_id(&task.id)), Style::default().fg(t.dim)),
                Span::styled(
                    clip(&task.goal, area.width.saturating_sub(15) as usize),
                    Style::default().fg(status_color(&task.status, t)),
                ),
            ]));
        }
    }

    // Section: Models (M5)
    if !state.models.is_empty() {
        section(&mut lines, "Models", t);
        for m in &state.models {
            let (sym, color) = if !m.online {
                ("·", t.fade)
            } else if m.quarantined {
                ("✗", t.error)
            } else {
                ("●", t.good)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{sym} "), Style::default().fg(color)),
                Span::styled(
                    short_model_name(&m.name),
                    Style::default().fg(t.body),
                ),
                Span::styled(
                    format!(" · {}", m.kind),
                    Style::default().fg(t.dim),
                ),
            ]));
        }
    }

    // Section: daemon
    section(&mut lines, "Daemon", t);
    lines.push(Line::from(vec![Span::styled(
        state.daemon_info.clone(),
        Style::default().fg(t.dim),
    )]));
    if state.running_tasks > 0 {
        lines.push(Line::from(vec![Span::styled(
            format!("running · {}", state.running_tasks),
            Style::default().fg(t.warn),
        )]));
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

fn section(out: &mut Vec<Line<'static>>, title: &str, t: &Theme) {
    if !out.is_empty() {
        out.push(Line::from(""));
    }
    out.push(Line::from(vec![Span::styled(
        format!("▼ {title}"),
        Style::default().fg(t.heading).add_modifier(Modifier::BOLD),
    )]));
}

fn kv(k: &str, v: &str, color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<9}"), Style::default().fg(ratatui::style::Color::Rgb(110, 110, 110))),
        Span::styled(v.to_string(), Style::default().fg(color)),
    ])
}

// ---------- Status + input lines ----------

fn render_status(f: &mut Frame, area: Rect, state: &AppState, t: &Theme) {
    let left = if let Some(err) = &state.error {
        Span::styled(format!(" ✗ {err}"), Style::default().fg(t.error))
    } else if let Some(s) = &state.status {
        Span::styled(format!(" ✓ {s}"), Style::default().fg(t.good))
    } else {
        Span::styled(
            format!(" jarvis · {}", state.daemon_url),
            Style::default().fg(t.dim),
        )
    };
    let hints = if state.tasks.is_empty() {
        "  type a goal, Enter to submit  ·  :  for commands  ·  ?  help "
    } else {
        "  j/k navigate  ·  Enter submit  ·  :cmd  ·  ?  help "
    };
    let right = Span::styled(hints, Style::default().fg(t.fade));

    // Fill: left + spacer + right
    let avail = area.width as usize;
    let lw = visual_len(&left.content);
    let rw = visual_len(&right.content);
    let pad = avail.saturating_sub(lw + rw);
    let line = Line::from(vec![left, Span::raw(" ".repeat(pad)), right]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_input(f: &mut Frame, area: Rect, state: &AppState, input: &Input, t: &Theme) {
    let prefix = " ❯ ";
    let prefix_style = Style::default().fg(t.accent).add_modifier(Modifier::BOLD);
    let placeholder = if state.selected_task().is_some() {
        "type to send  ·  :cancel  :all  :refresh  :theme  :q"
    } else {
        "describe a goal and press Enter"
    };
    let value = input.value();
    let text_span = if value.is_empty() {
        Span::styled(
            placeholder.to_string(),
            Style::default().fg(t.fade).add_modifier(Modifier::ITALIC),
        )
    } else {
        Span::styled(value.to_string(), Style::default().fg(t.focus))
    };
    let line = Line::from(vec![Span::styled(prefix, prefix_style), text_span]);
    f.render_widget(Paragraph::new(line), area);

    // Cursor positioning.
    let prefix_len = prefix.chars().count() as u16;
    let cursor_x = area.x + prefix_len + input.visual_cursor() as u16;
    let cursor_x = cursor_x.min(area.x + area.width.saturating_sub(1));
    f.set_cursor_position((cursor_x, area.y));
}

// ---------- Modals ----------

fn render_confirm_modal(f: &mut Frame, state: &AppState, t: &Theme) {
    let area = centered_rect(50, 3, f.area());
    f.render_widget(Clear, area);
    let id = state.selected_task().map(|t| short_id(&t.id)).unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(" Cancel task ", Style::default().fg(t.warn)),
        Span::styled(id, Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
        Span::raw("? "),
        Span::styled("[y]", Style::default().fg(t.error).add_modifier(Modifier::BOLD)),
        Span::raw(" / "),
        Span::styled("[n]", Style::default().fg(t.good)),
    ]);
    let p = Paragraph::new(vec![Line::from(""), line, Line::from("")]);
    f.render_widget(p, area);
}

fn render_help_modal(f: &mut Frame, t: &Theme) {
    let area = centered_rect(60, 18, f.area());
    f.render_widget(Clear, area);
    let lines = vec![
        Line::from(vec![Span::styled(
            "Jarvis keybinds",
            Style::default().fg(t.heading).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled("Navigation", Style::default().fg(t.accent))]),
        Line::from("  ↑/↓, j/k     previous / next task"),
        Line::from("  r            refresh now"),
        Line::from("  a            toggle active / all"),
        Line::from(""),
        Line::from(vec![Span::styled("Compose", Style::default().fg(t.accent))]),
        Line::from("  type + Enter submits a new task"),
        Line::from("  :<command>   slash command (cancel, refresh, all, theme, q)"),
        Line::from(""),
        Line::from(vec![Span::styled("Actions", Style::default().fg(t.accent))]),
        Line::from("  c            cancel selected task"),
        Line::from("  ?            toggle this help"),
        Line::from("  q / Esc      quit"),
        Line::from(""),
        Line::from(vec![Span::styled("Press any key to dismiss.", Style::default().fg(t.dim))]),
    ];
    f.render_widget(Paragraph::new(lines), area);
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

// ---------- Payload extractors ----------

fn thought_of(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("thought")?.as_str().map(String::from)
}

fn tool_call_summary(json: &str) -> (String, String) {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (String::from("?"), json.to_string()),
    };
    let tool = v
        .get("tool")
        .and_then(|s| s.as_str())
        .unwrap_or("?")
        .to_string();
    let args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);
    let args_str = match tool.as_str() {
        "shell" => args.get("cmd").and_then(|s| s.as_str()).unwrap_or("").to_string(),
        "fs_read" | "fs_write" => args
            .get("path")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        _ => args.to_string(),
    };
    (tool, args_str)
}

fn tool_result_summary(json: &str) -> (String, String, bool) {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (json.to_string(), String::new(), true),
    };
    let summary = v
        .get("summary")
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_default();
    let exit = v
        .get("data")
        .and_then(|d| d.get("exit_code"))
        .map(|x| x.to_string())
        .map(|s| format!("exit={s}"))
        .unwrap_or_default();
    let ok = !v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
    (summary, exit, ok)
}

fn error_message(json: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return json.to_string(),
    };
    v.get("message")
        .or_else(|| v.get("error"))
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_else(|| json.to_string())
}

fn verdict_summary(json: &str) -> (String, String) {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (String::from("?"), json.to_string()),
    };
    let verdict = v
        .get("verdict")
        .and_then(|s| s.as_str())
        .unwrap_or("?")
        .to_string();
    let msg = v
        .get("message")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    (verdict, msg)
}

fn step_of(json: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("step")?.as_i64()
}

fn counters_for(events: &std::collections::VecDeque<Event>) -> (String, usize, Vec<(String, usize)>) {
    use std::collections::HashMap;
    let mut step = 0i64;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for ev in events {
        if ev.kind == "heartbeat"
            && let Some(s) = step_of(&ev.payload_json)
        {
            step = step.max(s);
        }
        *counts.entry(ev.kind.clone()).or_insert(0) += 1;
    }
    let mut kinds: Vec<_> = counts.into_iter().collect();
    kinds.sort_by_key(|b| std::cmp::Reverse(b.1));
    (step.to_string(), events.len(), kinds)
}

fn visual_len(s: &str) -> usize {
    s.chars().count()
}

fn short_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

/// "local:gemma" → "gemma"; "remote:deepseek_pro" → "deepseek_pro".
fn short_model_name(name: &str) -> String {
    name.split_once(':').map(|(_, n)| n.to_string()).unwrap_or_else(|| name.to_string())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[allow(dead_code)]
fn _silence_task_warning(_t: &Task) {}
