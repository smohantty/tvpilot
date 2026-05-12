//! Top-level dispatcher for the `tvpilot` multicall binary.
//!
//! `tvpilot daemon`        → daemon mode (long-lived, holds AT-SPI conn)
//! `tvpilot <other verb>`  → CLI mode (one request to the running daemon)

use anyhow::Result;

mod cli;
mod daemon;
mod proto;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    match first.as_deref().and_then(|s| s.to_str()) {
        Some("daemon") => daemon::run().await,
        _ => {
            let rest: Vec<_> = std::env::args_os().skip(1).collect();
            cli::run(rest).await
        }
    }
}
