//! CLI mode: a thin postcard client over the daemon's unix socket.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::proto::{Command, Payload, Request, Response};

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/5001".into());
    PathBuf::from(format!("{}/tvpilot.sock", runtime))
}

pub async fn run<I: IntoIterator<Item = std::ffi::OsString>>(args: I) -> Result<()> {
    let args: Vec<String> = args
        .into_iter()
        .filter_map(|s| s.into_string().ok())
        .collect();
    let cmd = parse_args(&args)?;
    let resp = send_command(&cmd).await?;
    render(resp);
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Command> {
    let mut iter = args.iter();
    let verb = iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: tvpilot <snap|ping|close> [...]"))?;
    match verb.as_str() {
        "ping" => Ok(Command::Ping),
        "close" => Ok(Command::Close),
        "snap" => {
            let mut interactive = false;
            let mut verbose = false;
            for arg in iter {
                match arg.as_str() {
                    "-i" | "--interactive" => interactive = true,
                    "-v" | "--verbose" => verbose = true,
                    other => anyhow::bail!("unknown flag for snap: {}", other),
                }
            }
            Ok(Command::Snap {
                interactive,
                verbose,
            })
        }
        _ => Err(anyhow::anyhow!("unknown verb: {}", verb)),
    }
}

async fn send_command(cmd: &Command) -> Result<Response> {
    let sock = socket_path();
    let mut stream = UnixStream::connect(&sock)
        .await
        .with_context(|| format!("connect {} — is the daemon running?", sock.display()))?;

    let req = Request {
        rid: 1,
        cmd: clone_command(cmd),
    };
    let body = postcard::to_allocvec(&req).context("encode request")?;
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.context("write length")?;
    stream.write_all(&body).await.context("write body")?;
    stream.flush().await.ok();

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("read response length")?;
    let resp_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; resp_len];
    stream
        .read_exact(&mut buf)
        .await
        .context("read response body")?;
    let resp: Response = postcard::from_bytes(&buf).context("decode response")?;
    Ok(resp)
}

fn clone_command(c: &Command) -> Command {
    match c {
        Command::Ping => Command::Ping,
        Command::Close => Command::Close,
        Command::Snap {
            interactive,
            verbose,
        } => Command::Snap {
            interactive: *interactive,
            verbose: *verbose,
        },
    }
}

fn render(resp: Response) {
    match resp.payload {
        Payload::Pong { uptime_ms } => {
            println!("pong (daemon up {}ms)", uptime_ms);
        }
        Payload::Closed => {
            println!("daemon closed");
        }
        Payload::Error(msg) => {
            eprintln!("error: {}", msg);
            std::process::exit(1);
        }
        Payload::Snap(s) => {
            eprintln!(
                "[tvpilot] app={} nodes={} detect={}ms walk={}ms total={}ms",
                s.app_bus, s.node_count, resp.timing.detect_ms, resp.timing.walk_ms, resp.timing.total_ms
            );
            print!("{}", s.rendered);
        }
    }
}
