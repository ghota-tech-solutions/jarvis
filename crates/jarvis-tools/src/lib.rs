//! jarvis-tools — Tool trait + built-in tools.
//!
//! M2 ships three: `fs_read`, `fs_write`, `shell`. All operate WITHOUT sandboxing
//! (M3 adds Docker/Native sandbox backends). Use within trusted workspaces only.

pub mod apply_patch;
pub mod fetch_url;
mod gh;
pub mod git_status;
pub mod list_dir;
pub mod read_multiple;
mod registry;
mod request_edit;
mod search;
mod search_tools;
mod spawn_subagent;
pub mod surgical_edit;
mod tool;
mod tools;
pub mod view_symbols;
mod web_search;

pub use fetch_url::FetchUrlTool;
pub use gh::{GhPrCommentTool, GhPrCreateTool, GhPrListTool, GhPrViewTool};
pub use git_status::GitStatusTool;
pub use list_dir::ListDirTool;
pub use read_multiple::FsReadManyTool;
pub use registry::ToolRegistry;
pub use request_edit::RequestEditTool;
pub use search::{GlobTool, GrepTool};
pub use search_tools::SearchToolsTool;
pub use spawn_subagent::SpawnSubagentTool;
pub use surgical_edit::ReplaceFileContentTool;
pub use tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSchema};
pub use tools::{ApplyPatchTool, FsReadTool, FsWriteTool, ShellTool, UpdatePlanTool};
pub use view_symbols::ViewSymbolsTool;
pub use web_search::WebSearchTool;
