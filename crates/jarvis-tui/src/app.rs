//! Shared application state.
//!
//! The TUI keeps a small in-memory mirror of (a) the daemon's task list, refreshed
//! by polling, and (b) the live event stream for the currently-selected task.

use crate::theme::{Theme, DARK_DEFAULT};
use jarvis_api::{jarvis_client::JarvisClient, Event, Task};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::transport::Channel;

pub const EVENTS_BUFFER_CAP: usize = 1000;
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2000);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    /// Default: input bar accepts text or single-key shortcuts.
    Normal,
    /// Confirming a destructive action (cancel).
    ConfirmCancel,
    /// Showing keybinds.
    Help,
}

#[derive(Debug)]
pub struct AppState {
    pub focus: Focus,
    pub tasks: Vec<Task>,
    pub selected: usize,
    /// Events for `selected_task_id()`. Bounded buffer.
    pub events: VecDeque<Event>,
    /// Set when a non-fatal error needs surfacing in the status bar.
    pub error: Option<String>,
    /// Status message (e.g. "submitted task abc"). Cleared on next user input.
    pub status: Option<String>,
    /// Daemon version + uptime, refreshed at boot.
    pub daemon_info: String,
    pub quit: bool,
    /// Toggled by 'a': true = show all (incl. finished), false = active only.
    pub show_all: bool,
    /// Active color theme.
    pub theme: Theme,
    /// Current daemon endpoint (for status bar).
    pub daemon_url: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            focus: Focus::Normal,
            tasks: Vec::new(),
            selected: 0,
            events: VecDeque::with_capacity(EVENTS_BUFFER_CAP),
            error: None,
            status: None,
            daemon_info: String::from("connecting…"),
            quit: false,
            show_all: false,
            theme: DARK_DEFAULT,
            daemon_url: String::new(),
        }
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }

    pub fn selected_task_id(&self) -> Option<String> {
        self.selected_task().map(|t| t.id.clone())
    }

    pub fn next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.tasks.len() - 1);
    }

    pub fn prev(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn push_event(&mut self, ev: Event) {
        if self.events.len() == EVENTS_BUFFER_CAP {
            self.events.pop_front();
        }
        self.events.push_back(ev);
    }

    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.error = None;
    }

    pub fn clear_messages(&mut self) {
        self.error = None;
        self.status = None;
    }
}

#[derive(Clone)]
pub struct App {
    pub state: Arc<Mutex<AppState>>,
    pub client: JarvisClient<Channel>,
}

impl App {
    pub fn new(channel: Channel, daemon_url: String) -> Self {
        let mut s = AppState::new();
        s.daemon_url = daemon_url;
        Self {
            state: Arc::new(Mutex::new(s)),
            client: JarvisClient::new(channel),
        }
    }
}
