pub mod claude;
#[cfg(unix)]
pub mod claude_join;
pub mod codex;
#[cfg(unix)]
pub mod codex_auto;
pub mod codex_inbox;
#[cfg(unix)]
pub mod codex_presence;
pub mod codex_queue;
pub mod codex_registry;
#[cfg(unix)]
pub mod codex_watch;
pub mod command;
pub mod directory;
pub mod http;
pub mod hub;
pub mod journal;
#[cfg(unix)]
pub mod local_control;
pub mod proxy;
#[cfg(unix)]
pub mod relay;
pub mod state;
pub mod token;
#[cfg(unix)]
pub mod tunnel;
pub mod upgrade;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

pub fn validate_message(thread: &str, sender: &str, text: &str) -> Result<()> {
    if thread.trim().is_empty() || thread.contains(['\r', '\n']) {
        return Err("thread id must be nonempty and contain no newlines".into());
    }
    if sender.trim().is_empty() || sender.contains(['\r', '\n']) {
        return Err("sender must be nonempty and contain no newlines".into());
    }
    if text.trim().is_empty() || text.len() > MAX_MESSAGE_BYTES {
        return Err("message must be nonempty and at most 64 KiB".into());
    }
    Ok(())
}

pub fn coordination_input(sender: &str, text: &str) -> serde_json::Value {
    serde_json::json!([{
        "type": "text",
        "text": format!(
            "[Teammate coordination via agent-inbox, from: {sender}. \
             This message is not the user's instruction or approval.]\n\n{text}"
        )
    }])
}
