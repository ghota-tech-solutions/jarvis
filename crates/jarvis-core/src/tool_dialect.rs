//! Tool-call dialect tag — describes the contract a given model uses when
//! emitting tool calls and finalizing tasks.
//!
//! The agent loop builds the system prompt and (optionally) preprocesses
//! the raw model response based on this tag, so different families can be
//! mixed in the same registry without forcing a lowest-common-denominator
//! prompt.
//!
//! Variants:
//!
//! - `Json` (default) — the universal contract: one JSON object per reply,
//!   `{thought, action, tool?, args?, message?}`. Works on any
//!   instruction-tuned model that follows a system prompt.
//! - `Gemma4Strict` — same JSON contract but with an extra-tight system
//!   prompt: explicit "no markdown, no prose, JSON only" + one-shot
//!   example + an anti-drift recovery clause. Recommended for any Gemma
//!   family model served via OpenAI-compat (MLX-LM, llama.cpp, vLLM
//!   without `--tool-call-parser gemma4`).
//! - `Gemma4Native` — opt-in only. The model emits its native
//!   `<|tool_call>call:fn{k:<|"|>v<|"|>}<tool_call|>` envelopes (see § C.M-C);
//!   a preprocessor rewrites them to the JSON contract before the parser
//!   sees them. Only useful with backends that expose those special
//!   tokens (Ollama `gemma4:*`, vLLM with `--tool-call-parser gemma4`).
//! - `HermesXml` — Hermes / Qwen 2.5/3 / some Mistral fine-tunes wrap a
//!   JSON tool call in `<tool_call>{"name":"fn","arguments":{...}}</tool_call>`.
//! - `LlamaPython` — Llama 3.1+ "python-tag" envelope:
//!   `<|python_tag|>fn(arg1="value", arg2=42)<|eom_id|>`.
//! - `ToolCodeBlock` — generic code-block dialect:
//!   ` ```tool_code\nfn(arg1="value")\n``` ` (also accepts ` ```python ` as
//!   a fallback when the body looks like a tool call).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDialect {
    #[default]
    Json,
    Gemma4Strict,
    Gemma4Native,
    HermesXml,
    LlamaPython,
    ToolCodeBlock,
}

impl ToolDialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Gemma4Strict => "gemma4_strict",
            Self::Gemma4Native => "gemma4_native",
            Self::HermesXml => "hermes_xml",
            Self::LlamaPython => "llama_python",
            Self::ToolCodeBlock => "tool_code_block",
        }
    }
}

impl std::fmt::Display for ToolDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_json() {
        assert_eq!(ToolDialect::default(), ToolDialect::Json);
    }

    #[test]
    fn serde_roundtrip_snake_case() {
        let d = ToolDialect::Gemma4Strict;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"gemma4_strict\"");
        let back: ToolDialect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn deserializes_from_toml_string() {
        // Mirrors how the per-model jarvis.toml block carries the tag.
        let s = "\"gemma4_native\"";
        let d: ToolDialect = serde_json::from_str(s).unwrap();
        assert_eq!(d, ToolDialect::Gemma4Native);
    }

    #[test]
    fn serde_roundtrip_hermes_xml() {
        let d = ToolDialect::HermesXml;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"hermes_xml\"");
        let back: ToolDialect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        assert_eq!(d.as_str(), "hermes_xml");
    }

    #[test]
    fn serde_roundtrip_llama_python() {
        let d = ToolDialect::LlamaPython;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"llama_python\"");
        let back: ToolDialect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        assert_eq!(d.as_str(), "llama_python");
    }

    #[test]
    fn serde_roundtrip_tool_code_block() {
        let d = ToolDialect::ToolCodeBlock;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"tool_code_block\"");
        let back: ToolDialect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        assert_eq!(d.as_str(), "tool_code_block");
    }
}
