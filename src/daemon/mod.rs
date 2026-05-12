//! Daemon mode: bind a unix socket and serve framed Snap requests.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::proto::{Command, Payload, Request, Response, SnapResult, Timing};

mod atspi;

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/5001".into());
    PathBuf::from(format!("{}/tvpilot.sock", runtime))
}

pub async fn run() -> Result<()> {
    let sock = socket_path();
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("bind {}", sock.display()))?;
    eprintln!("[tvpilotd] listening on {}", sock.display());

    // Toggle a11y on at startup. Best-effort.
    if let Err(e) = atspi::enable_a11y().await {
        eprintln!("[tvpilotd] WARN enable_a11y: {:#}", e);
    } else {
        eprintln!("[tvpilotd] a11y enabled");
    }

    // Hold a single warm AT-SPI connection for the daemon's lifetime.
    let atspi_conn = atspi::connect_atspi().await.context("connect at-spi")?;
    let started = Instant::now();
    eprintln!("[tvpilotd] ready");

    loop {
        let (stream, _) = listener.accept().await.context("accept")?;
        let atspi = atspi_conn.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, atspi, started).await {
                eprintln!("[tvpilotd] connection error: {:#}", e);
            }
        });
    }
}

async fn handle(
    mut stream: UnixStream,
    atspi: Arc<dbus::nonblock::SyncConnection>,
    started: Instant,
) -> Result<()> {
    loop {
        let req = match read_frame::<Request>(&mut stream).await {
            Ok(r) => r,
            Err(_) => return Ok(()), // peer closed
        };
        let resp = dispatch(req, &atspi, started).await;
        let close = matches!(&resp.payload, Payload::Closed);
        write_frame(&mut stream, &resp).await?;
        if close {
            stream.shutdown().await.ok();
            std::process::exit(0);
        }
    }
}

async fn dispatch(
    req: Request,
    atspi: &Arc<dbus::nonblock::SyncConnection>,
    started: Instant,
) -> Response {
    let t0 = Instant::now();
    let rid = req.rid;
    let mut timing = Timing::default();

    let payload = match req.cmd {
        Command::Ping => Payload::Pong {
            uptime_ms: started.elapsed().as_millis() as u64,
        },
        Command::Close => Payload::Closed,
        Command::Snap { interactive, verbose } => {
            match do_snap(atspi, interactive, verbose, &mut timing).await {
                Ok(r) => Payload::Snap(r),
                Err(e) => Payload::Error(format!("{:#}", e)),
            }
        }
    };
    timing.total_ms = t0.elapsed().as_millis() as u32;
    Response { rid, payload, timing }
}

async fn do_snap(
    atspi: &Arc<dbus::nonblock::SyncConnection>,
    interactive: bool,
    verbose: bool,
    timing: &mut Timing,
) -> Result<SnapResult> {
    let apps = atspi::list_apps(atspi).await?;
    eprintln!("[tvpilotd] {} apps on a11y bus", apps.len());

    let t = Instant::now();
    let active = atspi::find_active_app(atspi, &apps).await;
    timing.detect_ms = t.elapsed().as_millis() as u32;

    // Walk targets: active app if we found one, otherwise every app
    // (the SHOWING prune inside the walker will collapse background apps
    // to empty subtrees cheaply).
    let targets: Vec<(String, String)> = match &active {
        Some(a) => {
            eprintln!("[tvpilotd] active app: {}", a.0);
            vec![a.clone()]
        }
        None => {
            eprintln!(
                "[tvpilotd] no STATE_ACTIVE found; walking all {} apps",
                apps.len()
            );
            apps.clone()
        }
    };

    let t = Instant::now();
    let walks: Vec<_> = targets
        .iter()
        .map(|(s, p)| {
            atspi::walk(
                atspi.clone(),
                s.clone(),
                p.clone(),
                interactive,
                verbose,
            )
        })
        .collect();
    let results = futures::future::join_all(walks).await;
    timing.walk_ms = t.elapsed().as_millis() as u32;

    let mut rendered = String::new();
    let mut node_count = 0u32;
    let mut chosen_bus = String::new();
    let mut chosen_nodes = 0u32;
    for ((sender, _), (r, c)) in targets.iter().zip(results.iter()) {
        node_count += c;
        if *c == 0 {
            continue;
        }
        // Prefer the app with the most nodes as the "primary".
        if *c > chosen_nodes {
            chosen_bus = sender.clone();
            chosen_nodes = *c;
        }
        if !r.is_empty() {
            rendered.push_str(&format!("# app={}\n", sender));
            rendered.push_str(r);
        }
    }

    Ok(SnapResult {
        app_bus: if chosen_bus.is_empty() {
            "<none>".to_string()
        } else {
            chosen_bus
        },
        node_count,
        rendered,
    })
}

async fn read_frame<T: for<'de> serde::Deserialize<'de>>(s: &mut UnixStream) -> Result<T> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf).await.context("read length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("frame too big: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await.context("read body")?;
    postcard::from_bytes(&buf).context("postcard decode")
}

async fn write_frame<T: serde::Serialize>(s: &mut UnixStream, v: &T) -> Result<()> {
    let body = postcard::to_allocvec(v).context("postcard encode")?;
    let len = (body.len() as u32).to_le_bytes();
    s.write_all(&len).await.context("write length")?;
    s.write_all(&body).await.context("write body")?;
    s.flush().await.context("flush")?;
    Ok(())
}
