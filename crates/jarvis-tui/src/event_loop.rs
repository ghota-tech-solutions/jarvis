//! Main event loop: terminal init, render tick, key handling, daemon polling.

use crate::app::{App, AppState, Focus, POLL_INTERVAL};
use crate::ui;
use anyhow::{Context, Result};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use jarvis_api::{
    jarvis_client::JarvisClient, ListTasksRequest, PingRequest, StreamEventsRequest, TaskHandle,
    TaskSpec,
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
    // The currently-running events stream task gets its own cancel token so we
    // can swap stream targets when the user selects a different task.
    let events_cancel = Arc::new(Mutex::new(CancellationToken::new()));

    // Initial fetches: daemon info + first task list.
    {
        let mut client = app.client.clone();
        let state = app.state.clone();
        if let Ok(info) = client.ping(PingRequest {}).await {
            let info = info.into_inner();
            let mut s = state.lock().await;
            s.daemon_info = format!("daemon v{} (up {}s)", info.version, info.uptime_seconds);
        }
        refresh_tasks(&mut client, &state).await;
    }

    // Background pollers.
    spawn_task_poller(app.clone(), global_cancel.clone());
    spawn_event_streamer(app.clone(), events_cancel.clone(), global_cancel.clone());

    let mut input = Input::default();
    let mut terminal_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(150));

    loop {
        // Render. We snapshot state under lock to keep render quick.
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
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
    )?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("ratatui::Terminal::new")
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    )?;
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
        Focus::Tasks => handle_tasks_key(k, app, events_cancel).await,
        Focus::NewTask => handle_new_task_key(k, app, input, events_cancel).await,
        Focus::ConfirmCancel => handle_confirm_key(k, app).await,
        Focus::Help => {
            // any key dismisses
            app.state.lock().await.focus = Focus::Tasks;
            Ok(())
        }
    }
}

async fn handle_tasks_key(
    k: KeyEvent,
    app: &App,
    events_cancel: &Arc<Mutex<CancellationToken>>,
) -> Result<()> {
    match (k.code, k.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
            app.state.lock().await.quit = true;
        }
        (KeyCode::Char('?'), _) => {
            app.state.lock().await.focus = Focus::Help;
        }
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
            let mut s = app.state.lock().await;
            let prev_sel = s.selected_task_id();
            s.next();
            if s.selected_task_id() != prev_sel {
                s.clear_events();
                rotate_event_stream(events_cancel).await;
            }
        }
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
            let mut s = app.state.lock().await;
            let prev_sel = s.selected_task_id();
            s.prev();
            if s.selected_task_id() != prev_sel {
                s.clear_events();
                rotate_event_stream(events_cancel).await;
            }
        }
        (KeyCode::Char('a'), _) => {
            let mut s = app.state.lock().await;
            s.show_all = !s.show_all;
            s.clear_messages();
        }
        (KeyCode::Char('r'), _) => {
            let mut client = app.client.clone();
            let state = app.state.clone();
            tokio::spawn(async move {
                refresh_tasks(&mut client, &state).await;
            });
        }
        (KeyCode::Char('n'), _) => {
            app.state.lock().await.focus = Focus::NewTask;
        }
        (KeyCode::Char('c'), _) => {
            let id = { app.state.lock().await.selected_task_id() };
            if id.is_some() {
                app.state.lock().await.focus = Focus::ConfirmCancel;
            } else {
                app.state.lock().await.set_error("no task selected");
            }
        }
        _ => {}
    }
    Ok(())
}

async fn handle_new_task_key(
    k: KeyEvent,
    app: &App,
    input: &mut Input,
    events_cancel: &Arc<Mutex<CancellationToken>>,
) -> Result<()> {
    match k.code {
        KeyCode::Esc => {
            app.state.lock().await.focus = Focus::Tasks;
            *input = Input::default();
        }
        KeyCode::Enter => {
            let goal = input.value().to_string();
            *input = Input::default();
            app.state.lock().await.focus = Focus::Tasks;
            if goal.trim().is_empty() {
                app.state.lock().await.set_error("empty goal");
                return Ok(());
            }
            submit_task(app, goal, events_cancel.clone()).await;
        }
        _ => {
            input.handle_event(&CtEvent::Key(k));
        }
    }
    Ok(())
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
            app.state.lock().await.focus = Focus::Tasks;
        }
        _ => {
            app.state.lock().await.focus = Focus::Tasks;
        }
    }
    Ok(())
}

async fn submit_task(app: &App, goal: String, events_cancel: Arc<Mutex<CancellationToken>>) {
    let mut client = app.client.clone();
    let state = app.state.clone();
    let workdir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    tokio::spawn(async move {
        let spec = TaskSpec {
            goal,
            workdir,
            max_steps: 20,
            sandbox: String::new(),
            net_policy: String::new(),
            use_worktree: false,
            base_ref: String::new(),
        };
        match client.submit_task(spec).await {
            Ok(h) => {
                let id = h.into_inner().id;
                state.lock().await.set_status(format!("submitted {}", short(&id)));
                // Refresh so the new task appears, then auto-select it.
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

fn spawn_event_streamer(
    app: App,
    events_cancel: Arc<Mutex<CancellationToken>>,
    global_cancel: CancellationToken,
) {
    let App { state, client } = app;
    tokio::spawn(async move {
        loop {
            // Snapshot the current cancel token + selected task.
            let (this_round_token, selected) = {
                let token = events_cancel.lock().await.clone();
                let s = state.lock().await;
                (token, s.selected_task_id())
            };
            // If no task is selected, just wait until selection changes (or global cancel).
            let Some(task_id) = selected else {
                tokio::select! {
                    _ = this_round_token.cancelled() => continue,
                    _ = global_cancel.cancelled() => return,
                }
            };

            // Start a fresh follow stream for this task.
            let mut client = client.clone();
            let stream_res = client
                .stream_events(StreamEventsRequest {
                    task_id: task_id.clone(),
                    follow: true,
                    since_id: 0,
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
                            // Only push if it's still the selected task (concurrent change race).
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
            // Preserve selected id across refresh.
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
