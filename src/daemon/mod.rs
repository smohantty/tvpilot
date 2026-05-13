//! Daemon mode: bind a unix socket and serve framed Snap requests.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::proto::{Command, Payload, Request, Response, SnapResult, Timing};

mod atspi;
mod input;

use atspi::{FocusPointer, RefEntry};
use input::{KeyInjector, resolve_key_name};

/// State shared across all connections.
struct DaemonState {
    atspi: Arc<dbus::nonblock::SyncConnection>,
    keys: Option<Arc<KeyInjector>>,
    // RefMap from the most recent snapshot. Click looks refs up here.
    refmap: Mutex<Vec<RefEntry>>,
    // Element most recently flagged as focused/highlighted via AT-SPI
    // signals. Compensates for Tizen Dali widgets not setting STATE_FOCUSED
    // or STATE_HIGHLIGHTED in `GetState` polls.
    last_focus: FocusPointer,
    started: Instant,
}

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

    // Best-effort key injector. Initialization can fail if the daemon doesn't
    // have the right Tizen capability; in that case `key`/`click` will error
    // out but `snap` still works.
    let keys = match KeyInjector::new() {
        Ok(k) => {
            eprintln!("[tvpilotd] key injector ready");
            Some(Arc::new(k))
        }
        Err(e) => {
            eprintln!("[tvpilotd] WARN no key injector: {:#}", e);
            None
        }
    };

    let last_focus: FocusPointer = Arc::new(Mutex::new(None));
    if let Err(e) = atspi::install_focus_listener(&atspi_conn, last_focus.clone()).await {
        eprintln!("[tvpilotd] WARN focus listener: {:#}", e);
    } else {
        eprintln!("[tvpilotd] focus listener installed");
    }

    let state = Arc::new(DaemonState {
        atspi: atspi_conn,
        keys,
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
            Err(_) => return Ok(()), // peer closed
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
        Command::Snap { interactive, verbose } => {
            match do_snap(&state, interactive, verbose, &mut timing).await {
                Ok(r) => Payload::Snap(r),
                Err(e) => Payload::Error(format!("{:#}", e)),
            }
        }
        Command::Key { name, count } => match do_key(&state, &name, count) {
            Ok(c) => Payload::KeySent { count: c },
            Err(e) => Payload::Error(format!("{:#}", e)),
        },
        Command::Click { ref_id, interactive, verbose } => {
            match do_click(&state, &ref_id, interactive, verbose, &mut timing).await {
                Ok(r) => Payload::Snap(r),
                Err(e) => Payload::Error(format!("{:#}", e)),
            }
        }
    };
    timing.total_ms = t0.elapsed().as_millis() as u32;
    Response { rid, payload, timing }
}

fn do_key(state: &Arc<DaemonState>, name: &str, count: u32) -> Result<u32> {
    let inj = state
        .keys
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no key injector — daemon failed to init efl_util"))?;
    let key = resolve_key_name(name)?;
    inj.send_key(key, count)?;
    Ok(count.max(1))
}

async fn do_click(
    state: &Arc<DaemonState>,
    ref_id: &str,
    interactive: bool,
    verbose: bool,
    timing: &mut Timing,
) -> Result<SnapResult> {
    // Resolve `eN` → RefEntry from the most recent snapshot's RefMap.
    let target = {
        let map = state.refmap.lock().unwrap();
        let idx = parse_ref_id(ref_id)
            .ok_or_else(|| anyhow::anyhow!("malformed ref '{}': expected eN", ref_id))?;
        if idx == 0 || idx > map.len() {
            return Err(anyhow::anyhow!(
                "RefUnknown: '{}' not in current snapshot (snapshot has {} refs)",
                ref_id,
                map.len()
            ));
        }
        map[idx - 1].clone()
    };
    eprintln!(
        "[tvpilotd] click {} → {} {}",
        ref_id, target.sender, target.path
    );

    // Click ladder. Best-effort, fall through to the next rung on any failure.
    let mut path_used = "unknown".to_string();

    // Rung 1: already focused/highlighted? → send Enter.
    if let Ok(s) = atspi::get_state(&state.atspi, &target.sender, &target.path).await {
        if s.focused() {
            if let Some(inj) = state.keys.as_ref() {
                inj.send_key("Return", 1).ok();
                path_used = "focused+Enter".to_string();
            }
        }
    }

    // Rung 2: direct Action.click / Action.activate.
    if path_used == "unknown" {
        if let Some(idx) =
            atspi::find_action(&state.atspi, &target.sender, &target.path).await
        {
            match atspi::do_action(&state.atspi, &target.sender, &target.path, idx).await {
                Ok(true) => path_used = format!("DoAction({})", idx),
                Ok(false) => eprintln!("[tvpilotd] DoAction({}) returned false", idx),
                Err(e) => eprintln!("[tvpilotd] DoAction failed: {:#}", e),
            }
        }
    }

    // Rung 3: GrabHighlight, then Enter.
    if path_used == "unknown" {
        match atspi::grab_highlight(&state.atspi, &target.sender, &target.path).await {
            Ok(true) => {
                if let Some(inj) = state.keys.as_ref() {
                    inj.send_key("Return", 1).ok();
                }
                path_used = "GrabHighlight+Enter".to_string();
            }
            _ => {}
        }
    }

    // Rung 4: GrabFocus, then Enter.
    if path_used == "unknown" {
        match atspi::grab_focus(&state.atspi, &target.sender, &target.path).await {
            Ok(true) => {
                if let Some(inj) = state.keys.as_ref() {
                    inj.send_key("Return", 1).ok();
                }
                path_used = "GrabFocus+Enter".to_string();
            }
            _ => {}
        }
    }

    if path_used == "unknown" {
        return Err(anyhow::anyhow!(
            "all click ladder rungs failed for {} ({})",
            ref_id,
            target.role
        ));
    }
    eprintln!("[tvpilotd] click path: {}", path_used);

    // Give the UI a beat to settle. PLAN.md calls for waiting on
    // window:post-render; we'll get there once we wire up event subscription.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    do_snap(state, interactive, verbose, timing).await
}

fn parse_ref_id(s: &str) -> Option<usize> {
    let trimmed = s
        .strip_prefix("ref=")
        .or_else(|| s.strip_prefix('@'))
        .unwrap_or(s);
    let n = trimmed.strip_prefix('e')?;
    n.parse::<usize>().ok()
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

    // Publish the RefMap so subsequent `click`/`focus` can resolve refs.
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
