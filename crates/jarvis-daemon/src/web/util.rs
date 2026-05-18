//! Small helpers shared across web handlers: HTML escaping, URL/JSON encoding,
//! markdown → HTML rendering, time/age formatting, axum form parsers, and the
//! `AppError` type for unified error responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use jarvis_core::{RoutingPolicy, SandboxMode};
use jarvis_sandbox::{NetPolicy, SandboxKind};
use std::str::FromStr;

// -------- HTML / URL / JS encoding ---------------------------------------

/// HTML-escape the five characters that can break attribute and text contexts
/// (`& < > " '`). Returns a fresh String to keep call sites terse.
pub(super) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode for `application/x-www-form-urlencoded` / URL paths.
/// Unreserved chars per RFC 3986 (`A-Z a-z 0-9 - _ . ~`) pass through; the
/// rest is %xx-escaped from the UTF-8 bytes.
pub(super) fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            _ => {
                for b in c.to_string().as_bytes() {
                    let _ = write!(out, "%{b:02X}");
                }
            }
        }
    }
    out
}

/// Emit a JSON-quoted string suitable for inlining in an HTML `onclick`
/// attribute, so it round-trips through the HTML parser AND the JS parser.
/// We escape backslashes/quotes/control chars for JS, then HTML-escape the
/// whole token.
pub(super) fn json_string_literal(s: &str) -> String {
    use std::fmt::Write as _;
    let mut json = String::with_capacity(s.len() + 2);
    json.push('"');
    for c in s.chars() {
        match c {
            '\\' => json.push_str("\\\\"),
            '"' => json.push_str("\\\""),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(json, "\\u{:04x}", c as u32);
            }
            c => json.push(c),
        }
    }
    json.push('"');
    html_escape(&json)
}

// -------- Markdown -------------------------------------------------------

/// Render markdown into safe HTML. We use pulldown-cmark with strict options:
/// raw HTML in the source is dropped so untrusted content can't break out of
/// the `.md` span the CSS targets.
pub(super) fn render_markdown(src: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(src, opts);
    let safe = parser.filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)));
    let mut out = String::with_capacity(src.len() + 32);
    html::push_html(&mut out, safe);
    format!(r#"<span class="md">{out}</span>"#)
}

// -------- Identity helpers -----------------------------------------------

/// Short prefix of a UUID (first hyphen-separated segment) — used in the UI
/// for "task 7f3a2b1c" labels.
pub(super) fn short(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

/// Strip a `local:` / `remote:` prefix from a registered model name.
pub(super) fn short_model(name: &str) -> String {
    name.split_once(':')
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| name.to_string())
}

// -------- Time / age formatting -----------------------------------------

/// Format an absolute age in seconds as `7s`, `12m`, `3h`, `4d`, `2mo`, `1y`.
/// Used wherever the UI needs a human-readable elapsed value.
pub(super) fn format_age(secs: i64) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let s = secs.max(0);
    match s {
        s if s < MIN => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MIN),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < MONTH => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}mo", s / MONTH),
        s => format!("{}y", s / YEAR),
    }
}

/// Format a timestamp (in microseconds since the epoch) as time-since-now.
/// Delegates to `format_age` so we use one age-bucketing rule everywhere.
pub(super) fn relative_time(micros: i64) -> String {
    let now = chrono::Utc::now().timestamp_micros();
    let dt = (now - micros).max(0) / 1_000_000;
    format_age(dt)
}

// -------- Submit-form parsers (each handles the "empty → config default"
//          fall-through so call sites can pass form values verbatim) -------

pub(super) fn parse_sandbox_kind(
    s: &str,
    cfg: &jarvis_config::Config,
) -> Result<SandboxKind, AppError> {
    let s = if s.is_empty() {
        cfg.sandbox.default_backend.as_str()
    } else {
        s
    };
    SandboxKind::from_str(s).map_err(AppError::BadRequest)
}

pub(super) fn parse_net_policy(
    s: &str,
    cfg: &jarvis_config::Config,
) -> Result<NetPolicy, AppError> {
    let s = if s.is_empty() {
        cfg.sandbox.default_net_policy.as_str()
    } else {
        s
    };
    NetPolicy::from_str(s).map_err(AppError::BadRequest)
}

pub(super) fn parse_routing(s: &str, cfg: &jarvis_config::Config) -> RoutingPolicy {
    let s = if s.is_empty() {
        cfg.routing.default_policy.as_str()
    } else {
        s
    };
    crate::service::parse_routing_str(s).unwrap_or(RoutingPolicy::Auto)
}

pub(super) fn parse_sandbox_mode(s: &str, cfg: &jarvis_config::Config) -> SandboxMode {
    let s = if s.is_empty() {
        cfg.sandbox.default_mode.as_str()
    } else {
        s
    };
    s.parse().unwrap_or_default()
}

// -------- AppError + IntoResponse ----------------------------------------

#[derive(Debug)]
pub(super) enum AppError {
    BadRequest(String),
    NotFound,
    Internal(String),
    /// 303 redirect to the given path.
    Redirect(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
            AppError::Internal(m) => {
                (StatusCode::INTERNAL_SERVER_ERROR, m).into_response()
            }
            AppError::Redirect(path) => (
                StatusCode::SEE_OTHER,
                [
                    ("Location", path.as_str()),
                    ("HX-Redirect", path.as_str()),
                ],
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(
            json_string_literal(r#"a"b\c"#),
            r#"&quot;a\&quot;b\\c&quot;"#
        );
    }

    #[test]
    fn html_escape_handles_all_five() {
        assert_eq!(html_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&#39;f");
    }

    #[test]
    fn urlencode_passes_unreserved() {
        assert_eq!(urlencode("abc-_.~"), "abc-_.~");
    }

    #[test]
    fn urlencode_escapes_spaces_and_utf8() {
        assert_eq!(urlencode("a b"), "a%20b");
        // `é` is 2 bytes (0xc3 0xa9) in UTF-8.
        assert_eq!(urlencode("é"), "%C3%A9");
    }

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(125), "2m");
        assert_eq!(format_age(7200), "2h");
        assert_eq!(format_age(86400 * 2), "2d");
        assert_eq!(format_age(86400 * 30 * 4), "4mo");
        assert_eq!(format_age(86400 * 365 * 3), "3y");
    }

    #[test]
    fn short_extracts_uuid_segment() {
        assert_eq!(short("7f3a2b1c-d4e5-f6a7-b8c9-d0e1f2a3b4c5"), "7f3a2b1c");
        assert_eq!(short("noseparator"), "noseparator");
    }

    #[test]
    fn short_model_strips_prefix() {
        assert_eq!(short_model("local:gemma"), "gemma");
        assert_eq!(short_model("remote:deepseek_pro"), "deepseek_pro");
        assert_eq!(short_model("bare"), "bare");
    }
}
