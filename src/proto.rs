//! Wire protocol between the `tvpilot` CLI and the `tvpilot daemon`.
//!
//! Each frame is `u32 LE length` followed by `length` bytes of postcard.
//! A connection may carry many request/response pairs in order.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub rid: u32,
    pub cmd: Command,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Ping,
    Snap {
        interactive: bool,
        verbose: bool,
    },
    /// Inject a TV remote key via libcapi-ui-efl-util. `name` is a friendly
    /// verb ("down", "enter", "back", "volup", …) — the daemon maps it to the
    /// Tizen efl_util key string.
    Key {
        name: String,
        count: u32,
    },
    /// Click an element by its `eN` ref from the most recent snapshot.
    /// Runs the click ladder (PLAN.md): try direct action → highlight+Enter
    /// → focus+Enter. Returns a fresh snapshot.
    Click {
        ref_id: String,
        interactive: bool,
        verbose: bool,
    },
    Close,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub rid: u32,
    pub payload: Payload,
    pub timing: Timing,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Payload {
    Pong { uptime_ms: u64 },
    Snap(SnapResult),
    KeySent { count: u32 },
    Closed,
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapResult {
    pub app_bus: String,
    pub node_count: u32,
    pub rendered: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Timing {
    pub total_ms: u32,
    pub detect_ms: u32,
    pub walk_ms: u32,
}
