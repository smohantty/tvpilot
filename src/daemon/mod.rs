//! Daemon mode: bind a unix socket and serve framed Snap requests.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::proto::{Command, Payload, Request, Response, SnapResult, Timing};

mod atspi;

use atspi::{FocusPointer, RefEntry};

/// State shared across all connections.
struct DaemonState {
    atspi: Arc<dbus::nonblock::SyncConnection>,
    /// RefMap from the most recent snapshot. Kept around because the agent
    /// pipeline benefits from stable ref identities across a snap→inspect
    /// cycle even though we don't yet have an action surface that consumes
    /// them.
    #[allow(dead_code)]
    refmap: Mutex<Vec<RefEntry>>,
    /// Element most recently flagged as focused/highlighted via AT-SPI
    /// signals. Compensates for Tizen Dali widgets not setting
    /// STATE_FOCUSED or STATE_HIGHLIGHTED in `GetState` polls.
    last_focus: FocusPointer,
    started: Instant,
}

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/5001".into());
    PathBuf::from(format!("{}/tvpilot.sock", runtime))
}

enum BindOutcome {
    AnotherDaemonAlive,
    Io(std::io::Error),
}

/// Bind the daemon socket safely. Try bind directly; on AddrInUse, probe by
/// connect. If the existing socket accepts, another daemon is alive — return
/// without taking over. If it doesn't, the file is stale; unlink and retry.
async fn bind_socket(sock: &PathBuf) -> Result<UnixListener, BindOutcome> {
    match UnixListener::bind(sock) {
        Ok(l) => return Ok(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => match UnixStream::connect(sock).await
        {
            Ok(_) => return Err(BindOutcome::AnotherDaemonAlive),
            Err(_) => {
                eprintln!("[tvpilotd] stale socket at {}, reclaiming", sock.display());
                let _ = std::fs::remove_file(sock);
            }
        },
        Err(e) => return Err(BindOutcome::Io(e)),
    }
    UnixListener::bind(sock).map_err(BindOutcome::Io)
}

pub async fn run() -> Result<()> {
    let sock = socket_path();
    let listener = match bind_socket(&sock).await {
        Ok(l) => l,
        Err(BindOutcome::AnotherDaemonAlive) => {
            eprintln!(
                "[tvpilotd] another daemon is already alive on {} — exiting",
                sock.display()
            );
            return Ok(());
        }
        Err(BindOutcome::Io(e)) => return Err(e).context(format!("bind {}", sock.display())),
    };
    eprintln!("[tvpilotd] listening on {}", sock.display());

    // Attach to the already-running AT-SPI bus. The operator is expected to
    // have brought up a11y (e.g. via `app_launcher -s
    // org.tizen.aurum-bootstrap`) before tvpilot runs.
    let atspi_conn = atspi::connect_atspi().await.context("connect at-spi")?;

    let last_focus: FocusPointer = Arc::new(Mutex::new(None));
    if let Err(e) = atspi::install_focus_listener(&atspi_conn, last_focus.clone()).await {
        eprintln!("[tvpilotd] WARN focus listener: {:#}", e);
    } else {
        eprintln!("[tvpilotd] focus listener installed");
    }

    let state = Arc::new(DaemonState {
        atspi: atspi_conn,
        refmap: Mutex::new(Vec::new()),
        last_focus,
        started: Instant::now(),
    });
    eprintln!("[tvpilotd] ready");

    loop {
        let (stream, _) = listener.accept().await.context("accept")?;
        if let Err(e) = handle(stream, state.clone()).await {
            eprintln!("[tvpilotd] connection error: {:#}", e);
        }
    }
}

async fn handle(mut stream: UnixStream, state: Arc<DaemonState>) -> Result<()> {
    loop {
        let req = match read_frame::<Request>(&mut stream).await {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let resp = dispatch(req, state.clone()).await;
        let close = matches!(&resp.payload, Payload::Closed);
        write_frame(&mut stream, &resp).await?;
        if close {
            stream.shutdown().await.ok();
            let _ = std::fs::remove_file(socket_path());
            std::process::exit(0);
        }
    }
}

async fn dispatch(req: Request, state: Arc<DaemonState>) -> Response {
    let t0 = Instant::now();
    let rid = req.rid;
    let mut timing = Timing::default();

    let payload = match req.cmd {
        Command::Ping => Payload::Pong {
            uptime_ms: state.started.elapsed().as_millis() as u64,
        },
        Command::Close => Payload::Closed,
        Command::Snap {
            interactive,
            verbose,
        } => match do_snap(&state, interactive, verbose, &mut timing).await {
            Ok(r) => Payload::Snap(r),
            Err(e) => Payload::Error(format!("{:#}", e)),
        },
    };
    timing.total_ms = t0.elapsed().as_millis() as u32;
    Response {
        rid,
        payload,
        timing,
    }
}

async fn do_snap(
    state: &Arc<DaemonState>,
    interactive: bool,
    verbose: bool,
    timing: &mut Timing,
) -> Result<SnapResult> {
    let atspi = &state.atspi;
    let apps = atspi::list_apps(atspi).await?;
    eprintln!("[tvpilotd] {} apps on a11y bus", apps.len());

    let t = Instant::now();
    let active = atspi::find_active_app(atspi, &apps).await;
    timing.detect_ms = t.elapsed().as_millis() as u32;

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

    let focus_snapshot: Option<(String, String)> = state.last_focus.lock().unwrap().clone();
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
                focus_snapshot.clone(),
            )
        })
        .collect();
    let results = futures::future::join_all(walks).await;
    timing.walk_ms = t.elapsed().as_millis() as u32;

    let mut rendered = String::new();
    let mut node_count = 0u32;
    let mut chosen_bus = String::new();
    let mut chosen_nodes = 0u32;
    let mut combined_refmap: Vec<RefEntry> = Vec::new();
    for ((sender, _), (r, c, rm)) in targets.iter().zip(results.iter()) {
        node_count += c;
        if *c == 0 {
            continue;
        }
        if *c > chosen_nodes {
            chosen_bus = sender.clone();
            chosen_nodes = *c;
        }
        if !r.is_empty() {
            rendered.push_str(&format!("# app={}\n", sender));
            rendered.push_str(r);
        }
        combined_refmap.extend(rm.iter().cloned());
    }
    *state.refmap.lock().unwrap() = combined_refmap;

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
