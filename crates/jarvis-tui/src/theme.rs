//! Color theme. The default is borderless + soft greys + a warm cream accent,
//! mirroring OpenCode's restrained terminal aesthetic. Swap themes via `:theme <name>`.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Body text — the most common color on screen.
    pub body: Color,
    /// Slightly muted body for metadata, timestamps, separator dashes.
    pub dim: Color,
    /// Very muted, near-background, for tertiary info.
    pub fade: Color,
    /// Headings in the sidebar.
    pub heading: Color,
    /// Single accent color — tool names, "Connected" badges.
    pub accent: Color,
    /// Assistant prose (decision text).
    pub assistant: Color,
    /// Success / completed.
    pub good: Color,
    /// Errors / failures.
    pub error: Color,
    /// Warnings / running / pending.
    pub warn: Color,
    /// Focused input / cursor highlight.
    pub focus: Color,
}

pub const DARK_DEFAULT: Theme = Theme {
    body: Color::Rgb(196, 196, 196),
    dim: Color::Rgb(128, 128, 128),
    fade: Color::Rgb(80, 80, 80),
    heading: Color::Rgb(230, 220, 200),
    accent: Color::Rgb(212, 180, 120),
    assistant: Color::Rgb(180, 200, 220),
    good: Color::Rgb(140, 180, 110),
    error: Color::Rgb(220, 110, 90),
    warn: Color::Rgb(220, 175, 90),
    focus: Color::Rgb(240, 240, 240),
};

pub const LIGHT: Theme = Theme {
    body: Color::Rgb(50, 50, 50),
    dim: Color::Rgb(110, 110, 110),
    fade: Color::Rgb(170, 170, 170),
    heading: Color::Rgb(20, 20, 30),
    accent: Color::Rgb(120, 90, 40),
    assistant: Color::Rgb(40, 80, 120),
    good: Color::Rgb(60, 130, 60),
    error: Color::Rgb(170, 50, 40),
    warn: Color::Rgb(160, 115, 40),
    focus: Color::Rgb(0, 0, 0),
};

impl Theme {
    pub fn by_name(name: &str) -> Option<Theme> {
        match name.to_ascii_lowercase().as_str() {
            "dark" | "default" => Some(DARK_DEFAULT),
            "light" => Some(LIGHT),
            _ => None,
        }
    }
}

pub fn status_color(status: &str, t: &Theme) -> Color {
    match status {
        "running" => t.warn,
        "completed" => t.good,
        "failed" => t.error,
        "cancelled" => t.fade,
        _ => t.dim,
    }
}
