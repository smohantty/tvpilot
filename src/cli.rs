//! CLI mode: a thin postcard client over the daemon's unix socket.
//!
//! Hand-rolled arg parsing — clap is far too heavy for our six-verb surface
//! under `opt-level="z" + lto + strip + panic=abort`.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsString;
use std::fs::File;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::proto::{Command, Payload, Request, Response, socket_path};

const USAGE: &str = "usage: tvpilot <daemon|snap|click|key|ping|close> [...]";

pub async fn run<I: IntoIterator<Item = OsString>>(args: I) -> Result<()> {
    let args: Vec<String> = args.into_iter().filter_map(|s| s.into_string().ok()).collect();
    let cmd = parse_args(&args)?;
    let resp = send_command(cmd).await?;
    render(resp);
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Command> {
    let mut iter = args.iter().map(String::as_str);
    let verb = iter.next().ok_or_else(|| anyhow!(USAGE))?;

    match verb {
        "ping" => Ok(Command::Ping),
        "close" => Ok(Command::Close),
        "snap" => {
            let (interactive, verbose) = parse_iv_flags(iter, "snap")?;
            Ok(Command::Snap { interactive, verbose })
        }
        "key" => {
            let name = iter
                .next()
                .ok_or_else(|| anyhow!("usage: tvpilot key <name> [count]"))?
                .to_string();
            let count = match iter.next() {
                Some(s) => s.parse().map_err(|_| anyhow!("count must be a positive integer"))?,
                None => 1,
            };
            Ok(Command::Key { name, count })
        }
        "click" => {
            let ref_id = iter
                .next()
                .ok_or_else(|| anyhow!("usage: tvpilot click <eN> [-i] [-v]"))?
                .to_string();
            let (interactive, verbose) = parse_iv_flags(iter, "click")?;
            Ok(Command::Click { ref_id, interactive, verbose })
        }
        other => bail!("unknown verb: {other}"),
    }
}

/// Parse the `-i`/`-v` flag pair shared by `snap` and `click`.
fn parse_iv_flags<'a>(iter: impl Iterator<Item = &'a str>, verb: &str) -> Result<(bool, bool)> {
    let (mut interactive, mut verbose) = (false, false);
    for arg in iter {
        match arg {
            "-i" | "--interactive" => interactive = true,
            "-v" | "--verbose" => verbose = true,
            other => bail!("unknown flag for {verb}: {other}"),
        }
    }
    Ok((interactive, verbose))
}

async fn send_command(cmd: Command) -> Result<Response> {
    let sock = socket_path();
    let mut stream = match UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(_) => {
            // Daemon not running — auto-spawn and retry. Matches PLAN.md's
            // lifecycle: "CLI connects to socket → fails → re-execs
            // /proc/self/exe daemon as detached child → polls socket up to
            // 2 s with backoff → connects".
            eprintln!("[tvpilot] daemon not running, spawning...");
            spawn_daemon_detached().context("spawn daemon")?;
            wait_for_socket(&sock, Duration::from_secs(3))
                .await
                .context("daemon did not come up in time")?;
            UnixStream::connect(&sock).await.context("connect after spawn")?
        }
    };

    let req = Request { rid: 1, cmd };
    let body = postcard::to_allocvec(&req).context("encode request")?;
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.context("write length")?;
    stream.write_all(&body).await.context("write body")?;
    stream.flush().await.ok();

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.context("read response length")?;
    let resp_len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; resp_len];
    stream.read_exact(&mut buf).await.context("read response body")?;
    postcard::from_bytes(&buf).context("decode response")
}

fn spawn_daemon_detached() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("locate /proc/self/exe")?;

    // Pipe stdio to a log file so the daemon doesn't get SIGPIPE after the
    // CLI exits. Best-effort — fall back to /dev/null if the log file can't
    // be created.
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/5001".into());
    let log_path = format!("{runtime}/tvpilotd.log");
    let (stdout, stderr) = match File::create(&log_path) {
        Ok(f) => {
            let f2 = f.try_clone().ok();
            (
                Stdio::from(f),
                f2.map(Stdio::from).unwrap_or_else(Stdio::null),
            )
        }
        Err(_) => (Stdio::null(), Stdio::null()),
    };

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(stdout).stderr(stderr);

    // Detach from the parent's controlling terminal and process group. Without
    // setsid the calling shell waits on the daemon's FDs (or its session) and
    // the CLI invocation appears to hang.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn().context("Command::spawn daemon")?;
    Ok(())
}

async fn wait_for_socket(sock: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(20);
    while Instant::now() < deadline {
        if UnixStream::connect(sock).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(delay).await;
        if delay < Duration::from_millis(200) {
            delay *= 2;
        }
    }
    Err(anyhow!(
        "socket {} did not become connectable in {timeout:?}",
        sock.display(),
    ))
}

fn render(resp: Response) {
    match resp.payload {
        Payload::Pong { uptime_ms } => println!("pong (daemon up {uptime_ms}ms)"),
        Payload::Closed => println!("daemon closed"),
        Payload::Error(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
        Payload::Snap(s) => {
            eprintln!(
                "[tvpilot] app={} nodes={} detect={}ms walk={}ms total={}ms",
                s.app_bus,
                s.node_count,
                resp.timing.detect_ms,
                resp.timing.walk_ms,
                resp.timing.total_ms,
            );
            print!("{}", s.rendered);
        }
        Payload::KeySent { count } => {
            eprintln!("[tvpilot] sent {count} key event(s) in {}ms", resp.timing.total_ms);
        }
    }
}
