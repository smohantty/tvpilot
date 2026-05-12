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
