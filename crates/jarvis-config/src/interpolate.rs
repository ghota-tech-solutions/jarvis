//! Tiny ${VAR} / ${VAR:-default} interpolation against the process environment.
//! Sufficient for our needs; we don't support nested or escaped forms.

use std::env;
use std::fmt;

#[derive(Debug)]
pub enum InterpolateError {
    UndefinedVar(String),
    UnterminatedExpr(usize),
}

impl fmt::Display for InterpolateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterpolateError::UndefinedVar(name) => {
                write!(f, "environment variable `{name}` is not set")
            }
            InterpolateError::UnterminatedExpr(at) => {
                write!(f, "unterminated `${{...}}` expression at byte {at}")
            }
        }
    }
}

impl std::error::Error for InterpolateError {}

/// Process the input line-by-line. Lines whose first non-whitespace char is `#`
/// are passed through verbatim (TOML comments — we don't want to resolve
/// example variables there). Inside other lines:
///   - `$$` is a literal `$` (escape)
///   - `${VAR}` and `${VAR:-default}` are replaced from the env
pub fn interpolate_env(input: &str) -> Result<String, InterpolateError> {
    let mut out = String::with_capacity(input.len());
    let mut byte_offset = 0;
    for line in input.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            out.push_str(line);
        } else {
            interpolate_line(line, byte_offset, &mut out)?;
        }
        byte_offset += line.len();
    }
    Ok(out)
}

fn interpolate_line(line: &str, base: usize, out: &mut String) -> Result<(), InterpolateError> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Escape: `$$` → literal `$`
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i + 2;
            let Some(end_rel) = bytes[start..].iter().position(|&b| b == b'}') else {
                return Err(InterpolateError::UnterminatedExpr(base + i));
            };
            let end = start + end_rel;
            let expr = &line[start..end];
            let (name, default) = if let Some(idx) = expr.find(":-") {
                (&expr[..idx], Some(&expr[idx + 2..]))
            } else {
                (expr, None)
            };
            match env::var(name) {
                Ok(val) => out.push_str(&val),
                Err(_) => match default {
                    Some(d) => out.push_str(d),
                    None => return Err(InterpolateError::UndefinedVar(name.to_string())),
                },
            }
            i = end + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_no_vars() {
        assert_eq!(interpolate_env("hello world").unwrap(), "hello world");
    }

    #[test]
    fn substitutes_set_var() {
        unsafe {
            env::set_var("JARVIS_TEST_VAR", "replaced");
        }
        let out = interpolate_env("a=${JARVIS_TEST_VAR}/b").unwrap();
        assert_eq!(out, "a=replaced/b");
    }

    #[test]
    fn default_used_when_unset() {
        unsafe {
            env::remove_var("JARVIS_TEST_NOPE");
        }
        let out = interpolate_env("a=${JARVIS_TEST_NOPE:-fallback}").unwrap();
        assert_eq!(out, "a=fallback");
    }

    #[test]
    fn unterminated_fails() {
        assert!(interpolate_env("oops ${UNCLOSED").is_err());
    }

    #[test]
    fn missing_no_default_fails() {
        unsafe {
            env::remove_var("JARVIS_TEST_MISSING");
        }
        assert!(interpolate_env("${JARVIS_TEST_MISSING}").is_err());
    }

    #[test]
    fn comments_are_not_interpolated() {
        unsafe {
            env::remove_var("JARVIS_TEST_NEVER");
        }
        let input = "# comment with ${JARVIS_TEST_NEVER} unset var\nactual = \"value\"\n";
        let out = interpolate_env(input).unwrap();
        assert!(out.contains("${JARVIS_TEST_NEVER}"));
    }

    #[test]
    fn dollar_dollar_is_literal_dollar() {
        let out = interpolate_env("price = \"$$5\"\n").unwrap();
        assert_eq!(out, "price = \"$5\"\n");
    }
}
