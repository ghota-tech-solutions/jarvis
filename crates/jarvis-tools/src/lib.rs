//! jarvis-tools — Tool trait + built-in tools.
//!
//! M2 ships three: `fs_read`, `fs_write`, `shell`. All operate WITHOUT sandboxing
//! (M3 adds Docker/Native sandbox backends). Use within trusted workspaces only.

pub mod apply_patch;
mod registry;
mod search;
mod tool;
mod tools;
mod web_search;

pub use registry::ToolRegistry;
pub use search::{GlobTool, GrepTool};
pub use tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
pub use tools::{ApplyPatchTool, FsReadTool, FsWriteTool, ShellTool, UpdatePlanTool};
pub use web_search::WebSearchTool;
