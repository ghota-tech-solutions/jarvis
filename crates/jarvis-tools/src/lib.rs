//! jarvis-tools — Tool trait + built-in tools.
//!
//! M2 ships three: `fs_read`, `fs_write`, `shell`. All operate WITHOUT sandboxing
//! (M3 adds Docker/Native sandbox backends). Use within trusted workspaces only.

mod registry;
mod tool;
mod tools;

pub use registry::ToolRegistry;
pub use tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
pub use tools::{FsReadTool, FsWriteTool, ShellTool};
