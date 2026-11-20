use crate::clipboard::ClipboardFeedback;
use rmcp::{
    ServiceExt, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;
use tokio::sync::oneshot;

mod pipe;
pub use pipe::Window;

#[derive(Serialize, Deserialize)]
pub enum Edit {
    Append,
    Insert,
    Replace,
}

#[derive(Serialize, Deserialize)]
pub enum Command {
    Read,
    Edit {
        operation: Edit,
        text: String,
        revision: Option<u64>,
    },
    Copy {
        revision: Option<u64>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Error {
    code: String,
    message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    windows: Vec<pipe::WindowInfo>,
}

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            windows: Vec::new(),
        }
    }
}

pub type AgentResult<T> = std::result::Result<T, Error>;

pub enum Reply {
    Value(Value),
    Copy(oneshot::Receiver<ClipboardFeedback>),
}

impl Reply {
    async fn finish(self) -> AgentResult<Value> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Copy(feedback) => match feedback.await {
                Ok(ClipboardFeedback::Copied(count)) => Ok(json!({"characters": count})),
                _ => Err(Error::new(
                    "clipboard_failed",
                    "Could not write to the Windows clipboard.",
                )),
            },
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Target {
    /// Window ID from list_windows. Omit when exactly one window is available.
    window_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TextEdit {
    /// Window ID from list_windows. Omit when exactly one window is available.
    window_id: Option<String>,
    /// Text to write. Include a newline to separate the text from an existing note.
    text: String,
    /// Optional revision from read_note. A mismatch leaves the note untouched.
    revision: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Replacement {
    /// Window ID from list_windows. Omit when exactly one window is available.
    window_id: Option<String>,
    /// Complete replacement text.
    text: String,
    /// Required revision from read_note.
    revision: u64,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CopyNote {
    /// Window ID from list_windows. Omit when exactly one window is available.
    window_id: Option<String>,
    /// Optional revision from read_note. A mismatch leaves the clipboard untouched.
    revision: Option<u64>,
}

#[derive(Clone)]
struct Tools;

#[tool_router(server_handler)]
impl Tools {
    #[tool(
        description = "List open DFlow scratchpad windows and their IDs and process IDs.",
        annotations(read_only_hint = true)
    )]
    async fn list_windows(&self) -> CallToolResult {
        tool_result(
            pipe::windows()
                .await
                .map(|windows| json!({ "windows": windows })),
        )
    }

    #[tool(
        description = "Read the current DFlow note and its revision.",
        annotations(read_only_hint = true)
    )]
    async fn read_note(&self, Parameters(target): Parameters<Target>) -> CallToolResult {
        call(target.window_id, Command::Read).await
    }

    #[tool(
        description = "Append text to the end of a DFlow note without changing its existing text.",
        annotations(destructive_hint = false, idempotent_hint = false)
    )]
    async fn append_text(&self, Parameters(args): Parameters<TextEdit>) -> CallToolResult {
        edit(args, Edit::Append).await
    }

    #[tool(
        description = "Insert text at the current caret in a DFlow note.",
        annotations(destructive_hint = false, idempotent_hint = false)
    )]
    async fn insert_text(&self, Parameters(args): Parameters<TextEdit>) -> CallToolResult {
        edit(args, Edit::Insert).await
    }

    #[tool(
        description = "Replace the entire DFlow note. Read it first and pass its revision.",
        annotations(destructive_hint = true)
    )]
    async fn replace_note(&self, Parameters(args): Parameters<Replacement>) -> CallToolResult {
        edit(
            TextEdit {
                window_id: args.window_id,
                text: args.text,
                revision: Some(args.revision),
            },
            Edit::Replace,
        )
        .await
    }

    #[tool(
        description = "Copy the whole DFlow note to the Windows clipboard on request. This overwrites the clipboard.",
        annotations(destructive_hint = true)
    )]
    async fn copy_note(&self, Parameters(args): Parameters<CopyNote>) -> CallToolResult {
        call(
            args.window_id,
            Command::Copy {
                revision: args.revision,
            },
        )
        .await
    }
}

async fn edit(args: TextEdit, operation: Edit) -> CallToolResult {
    if args.text.len() > 256 * 1024 {
        return tool_result(Err(Error::new(
            "text_too_large",
            "Send at most 256 KiB of UTF-8 text per edit.",
        )));
    }
    call(
        args.window_id,
        Command::Edit {
            operation,
            text: args.text,
            revision: args.revision,
        },
    )
    .await
}

async fn call(window_id: Option<String>, command: Command) -> CallToolResult {
    tool_result(pipe::call(window_id.as_deref(), command).await)
}

fn tool_result(result: AgentResult<Value>) -> CallToolResult {
    match result {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::structured_error(json!(error)),
    }
}

pub fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

pub fn serve() -> io::Result<()> {
    runtime()?.block_on(async {
        let server = Tools.serve(stdio()).await.map_err(io::Error::other)?;
        server.waiting().await.map_err(io::Error::other)?;
        Ok(())
    })
}
