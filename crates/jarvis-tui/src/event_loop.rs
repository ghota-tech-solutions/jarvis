//! Main event loop: terminal init, render tick, key handling, daemon polling.
//!
//! Input model (OpenCode-inspired):
//!   * The input bar at the bottom is ALWAYS visible and ALWAYS captures characters.
//!   * If the input is empty, single keys (j/k/n/c/q/r/a/?/Up/Down) act as shortcuts.
//!   * Pressing Enter on an empty input does nothing.
//!   * Pressing Enter on a `:cmd` runs a slash command.
//!   * Pressing Enter on plain text submits it as a new task goal.

use crate::app::{App, AppState, Focus, POLL_INTERVAL};
use crate::theme::Theme;
use crate::ui;
use anyhow::{Context, Result};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use jarvis_api::{
    jarvis_client::JarvisClient, ListTasksRequest, PingRequest, StatusRequest, StreamEventsRequest,
    TaskHandle, TaskSpec,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::warn;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

pub async fn run(app: App) -> Result<()> {
    let mut terminal = setup_terminal().context("setup terminal")?;

    let global_cancel = CancellationToken::new();
    let events_cancel = Arc::new(Mutex::new(CancellationToken::new()));

    // Initial fetches: daemon info + first task list.
    {
        let mut client = app.client.clone();
        let state = app.state.clone();
        if let Ok(info) = client.ping(PingRequest {}).await {
            let info = info.into_inner();
            let mut s = state.lock().await;
            s.daemon_info = format!("v{} · up {}s", info.version, info.uptime_seconds);
        }
        refresh_tasks(&mut client, &state).await;
    }

    spawn_task_poller(app.clone(), global_cancel.clone());
    spawn_status_poller(app.clone(), global_cancel.clone());
    spawn_event_streamer(app.clone(), events_cancel.clone(), global_cancel.clone());

    let mut input = Input::default();
    let mut terminal_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(150));

    loop {
        {
            let state = app.state.lock().await;
            terminal.draw(|f| ui::render(f, &state, &input))?;
            if state.quit {
                drop(state);
                break;
            }
        }

        tokio::select! {
            ev = terminal_events.next() => {
                if let Some(Ok(ev)) = ev
                    && let Err(e) = handle_term_event(ev, &app, &mut input, &events_cancel).await {
                    warn!(error = %e, "event handler error");
                }
            }
            _ = tick.tick() => {}
            _ = global_cancel.cancelled() => break,
        }
    }

    global_cancel.cancel();
    restore_terminal(terminal)?;
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    // No mouse capture: lets the user select/copy text natively and scroll
    // with the wheel (the terminal sees the events instead of us swallowing them).
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("ratatui::Terminal::new")
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn handle_term_event(
    ev: CtEvent,
    app: &App,
    input: &mut Input,
    events_cancel: &Arc<Mutex<CancellationToken>>,
) -> Result<()> {
    let CtEvent::Key(k) = ev else { return Ok(()) };
    if k.kind != KeyEventKind::Press {
        return Ok(());
    }

    let focus = { app.state.lock().await.focus.clone() };
    match focus {
        Focus::Normal => handle_normal_key(k, app, input, events_cancel).await,
        Focus::ConfirmCancel => handle_confirm_key(k, app).await,
        Focus::Help => {
            app.state.lock().await.focus = Focus::Normal;
            Ok(())
        }
    }
}

async fn handle_normal_key(
    k: KeyEvent,
    app: &App,
    input: &mut Input,
    events_cancel: &Arc<Mutex<CancellationToken>>,
) -> Result<()> {
    let is_empty = input.value().is_empty();

    // Ctrl+C always quits.
    if matches!(k.code, KeyCode::Char('c')) && k.modifiers.contains(KeyModifiers::CONTROL) {
        app.state.lock().await.quit = true;
        return Ok(());
    }

    match k.code {
        KeyCode::Enter => {
            let raw = input.value().to_string();
            *input = Input::default();
            if raw.trim().is_empty() {
                return Ok(());
            }
            if let Some(cmd) = raw.strip_prefix(':') {
                run_slash_command(cmd.trim(), app, events_cancel).await;
            } else {
                // Default: continuation. If a task is selected, the new task
                // becomes its child (inherits workdir/sandbox/net). Use `:new`
                // for a fresh, unparented task.
                let parent = app.state.lock().await.selected_task_id();
                submit_task(app, raw, parent, events_cancel.clone()).await;
            }
            return Ok(());
        }
        KeyCode::Esc => {
            if is_empty {
                app.state.lock().await.quit = true;
            } else {
                *input = Input::default();
            }
            return Ok(());
        }
        // Navigation keys only fire on empty input — they don't conflict with
        // typing because arrows aren't characters.
        KeyCode::Up | KeyCode::Down if is_empty => {
            let down = matches!(k.code, KeyCode::Down);
            let mut s = app.state.lock().await;
            let prev_sel = s.selected_task_id();
            if down {
                s.next();
            } else {
                s.prev();
            }
            if s.selected_task_id() != prev_sel {
                s.clear_events();
                drop(s);
                rotate_event_stream(events_cancel).await;
            }
            return Ok(());
        }
        KeyCode::PageUp | KeyCode::PageDown if is_empty => {
            let down = matches!(k.code, KeyCode::PageDown);
            let mut s = app.state.lock().await;
            let prev_sel = s.selected_task_id();
            for _ in 0..10 {
                if down { s.next(); } else { s.prev(); }
            }
            if s.selected_task_id() != prev_sel {
                s.clear_events();
                drop(s);
                rotate_event_stream(events_cancel).await;
            }
            return Ok(());
        }
        _ => {}
    }

    // Anything else (including letters) is just text input.
    input.handle_event(&CtEvent::Key(k));
    Ok(())
}

async fn run_slash_command(
    cmd: &str,
    app: &App,
    events_cancel: &Arc<Mutex<CancellationToken>>,
) {
    let (head, rest) = match cmd.split_once(' ') {
        Some((h, r)) => (h, r.trim()),
        None => (cmd, ""),
    };
    match head {
        "q" | "quit" | "exit" => app.state.lock().await.quit = true,
        "cancel" => {
            let id = { app.state.lock().await.selected_task_id() };
            if let Some(id) = id {
                app.state.lock().await.focus = Focus::ConfirmCancel;
                let _ = rest;
                let _ = id;
            } else {
                app.state.lock().await.set_error("no task to cancel");
            }
        }
        "refresh" | "r" => {
            let mut client = app.client.clone();
            let state = app.state.clone();
            tokio::spawn(async move { refresh_tasks(&mut client, &state).await });
        }
        "all" => {
            let mut s = app.state.lock().await;
            s.show_all = !s.show_all;
            let msg = if s.show_all { "showing all tasks" } else { "showing active only" };
            s.set_status(msg);
        }
        "diffs" | "diff" => {
            let mut s = app.state.lock().await;
            s.diffs_open = !s.diffs_open;
            s.expanded_diffs.clear();
            let msg = if s.diffs_open { "diffs expanded by default" } else { "diffs collapsed by default" };
            s.set_status(msg);
        }
        "help" | "?" => app.state.lock().await.focus = Focus::Help,
        "theme" => {
            if let Some(th) = Theme::by_name(rest) {
                let mut s = app.state.lock().await;
                s.theme = th;
                s.set_status(format!("theme: {rest}"));
            } else {
                app.state
                    .lock()
                    .await
                    .set_error(format!("unknown theme `{rest}` (try dark | light)"));
            }
        }
        "new" => {
            if rest.is_empty() {
                app.state.lock().await.set_error("usage: :new <goal>");
            } else {
                // Explicit :new always starts a fresh, unparented conversation.
                submit_task(app, rest.to_string(), None, events_cancel.clone()).await;
            }
        }
        other => {
            app.state
                .lock()
                .await
                .set_error(format!("unknown command `:{other}`"));
        }
    }
}

async fn handle_confirm_key(k: KeyEvent, app: &App) -> Result<()> {
    match k.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let id = { app.state.lock().await.selected_task_id() };
            if let Some(id) = id {
                let mut client = app.client.clone();
                let state = app.state.clone();
                tokio::spawn(async move {
                    match client.cancel_task(TaskHandle { id: id.clone() }).await {
                        Ok(_) => state.lock().await.set_status(format!("cancelled {}", short(&id))),
                        Err(e) => state.lock().await.set_error(format!("cancel: {}", e.message())),
                    }
                });
            }
            app.state.lock().await.focus = Focus::Normal;
        }
        _ => {
            app.state.lock().await.focus = Focus::Normal;
        }
    }
    Ok(())
}

async fn submit_task(
    app: &App,
    goal: String,
    parent_task_id: Option<String>,
    events_cancel: Arc<Mutex<CancellationToken>>,
) {
    let mut client = app.client.clone();
    let state = app.state.clone();
    // When continuing a conversation we let the daemon inherit workdir from
    // the parent (sending empty here triggers inheritance).
    let workdir = if parent_task_id.is_some() {
        String::new()
    } else {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    tokio::spawn(async move {
        let spec = TaskSpec {
            goal,
            workdir,
            max_steps: 20,
            sandbox: String::new(),
            net_policy: String::new(),
            use_worktree: false,
            base_ref: String::new(),
            routing_policy: String::new(),
            require_caps: Vec::new(),
            parent_task_id: parent_task_id.unwrap_or_default(),
        };
        match client.submit_task(spec).await {
            Ok(h) => {
                let id = h.into_inner().id;
                state.lock().await.set_status(format!("submitted {}", short(&id)));
                refresh_tasks(&mut client, &state).await;
                {
                    let mut s = state.lock().await;
                    if let Some(idx) = s.tasks.iter().position(|t| t.id == id) {
                        s.selected = idx;
                        s.clear_events();
                    }
                }
                rotate_event_stream(&events_cancel).await;
            }
            Err(e) => state.lock().await.set_error(format!("submit: {}", e.message())),
        }
    });
}

async fn rotate_event_stream(events_cancel: &Arc<Mutex<CancellationToken>>) {
    let mut tok = events_cancel.lock().await;
    tok.cancel();
    *tok = CancellationToken::new();
}

fn spawn_task_poller(app: App, cancel: CancellationToken) {
    let App { state, mut client } = app;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => refresh_tasks(&mut client, &state).await,
                _ = cancel.cancelled() => return,
            }
        }
    });
}

fn spawn_status_poller(app: App, cancel: CancellationToken) {
    let App { state, mut client } = app;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Ok(resp) = client.get_status(StatusRequest {}).await {
                        let s = resp.into_inner();
                        let mut st = state.lock().await;
                        st.daemon_info = format!("v{} · up {}s", s.version, s.uptime_seconds);
                        st.models = s.models;
                        st.running_tasks = s.running_tasks;
                    }
                }
                _ = cancel.cancelled() => return,
            }
        }
    });
}

fn spawn_event_streamer(
    app: App,
    events_cancel: Arc<Mutex<CancellationToken>>,
    global_cancel: CancellationToken,
) {
    let App { state, client } = app;
    tokio::spawn(async move {
        loop {
            let (this_round_token, selected) = {
                let token = events_cancel.lock().await.clone();
                let s = state.lock().await;
                (token, s.selected_task_id())
            };
            let Some(task_id) = selected else {
                tokio::select! {
                    _ = this_round_token.cancelled() => continue,
                    _ = global_cancel.cancelled() => return,
                }
            };

            let mut client = client.clone();
            let stream_res = client
                .stream_events(StreamEventsRequest {
                    task_id: task_id.clone(),
                    follow: true,
                    since_id: 0,
                    include_ancestors: true,
                })
                .await;
            let mut stream = match stream_res {
                Ok(r) => r.into_inner(),
                Err(e) => {
                    state.lock().await.set_error(format!("stream: {}", e.message()));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            loop {
                tokio::select! {
                    item = stream.next() => match item {
                        Some(Ok(ev)) => {
                            let mut s = state.lock().await;
                            if s.selected_task_id().as_deref() == Some(&task_id) {
                                s.push_event(ev);
                            }
                        }
                        Some(Err(e)) => {
                            state.lock().await.set_error(format!("stream err: {}", e.message()));
                            break;
                        }
                        None => break,
                    },
                    _ = this_round_token.cancelled() => break,
                    _ = global_cancel.cancelled() => return,
                }
            }
        }
    });
}

async fn refresh_tasks(client: &mut JarvisClient<Channel>, state: &Arc<Mutex<AppState>>) {
    let show_all = { state.lock().await.show_all };
    match client
        .list_tasks(ListTasksRequest {
            include_finished: show_all,
            limit: 200,
        })
        .await
    {
        Ok(resp) => {
            let resp = resp.into_inner();
            let mut s = state.lock().await;
            let prev_id = s.selected_task_id();
            s.tasks = resp.tasks;
            if let Some(prev) = prev_id
                && let Some(idx) = s.tasks.iter().position(|t| t.id == prev)
            {
                s.selected = idx;
            } else {
                s.selected = s.selected.min(s.tasks.len().saturating_sub(1));
            }
        }
        Err(e) => {
            state
                .lock()
                .await
                .set_error(format!("list_tasks: {}", e.message()));
        }
    }
}

fn short(id: &str) -> &str {
    id.split('-').next().unwrap_or(id)
}
