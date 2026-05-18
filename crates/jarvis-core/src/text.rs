//! Small string helpers shared across crates. They live here so we have ONE
//! correct implementation of each instead of five subtly-different copies.

/// Clip `s` to at most `max` BYTES, walking back to a UTF-8 char boundary so
/// we never split a multi-byte codepoint. Appends a `[Nb truncated]` counter
/// so downstream code (and the agent reading observations) can tell how much
/// was dropped. Use this for byte-budget contexts: tool output, sandbox logs,
/// anything where total payload size is the constraint.
pub fn clip(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let end = floor_char_boundary(s, max_bytes);
    format!("{}…[{}b truncated]", &s[..end], s.len() - end)
}

/// Clip `s` to at most `max` CHARS, appending a single `…`. Use this for UI
/// labels (table cells, sidebar lines, log subjects) where visual width is the
/// constraint and the counter would be noise.
pub fn clip_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn floor_char_boundary(s: &str, mut len: usize) -> usize {
    len = len.min(s.len());
    while len > 0 && !s.is_char_boundary(len) {
        len -= 1;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_under_cap_is_passthrough() {
        assert_eq!(clip("hello", 10), "hello");
    }

    #[test]
    fn clip_respects_char_boundary() {
        // `aé` = 3 bytes: `a`(1) + `é`(2). max=2 must NOT split — clip to byte 1.
        let out = clip("aé", 2);
        assert!(out.starts_with("a…"));
        assert!(!out.contains('\u{c3}'));
    }

    #[test]
    fn clip_chars_counts_chars_not_bytes() {
        // `éééé` = 8 bytes but 4 chars. max_chars=3 → 2 chars + ellipsis.
        let out = clip_chars("éééé", 3);
        assert_eq!(out, "éé…");
    }

    #[test]
    fn clip_chars_under_cap_is_passthrough() {
        assert_eq!(clip_chars("hi", 5), "hi");
    }
}
