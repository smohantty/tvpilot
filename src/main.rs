use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use futures::future::{BoxFuture, FutureExt};
use std::time::Instant;
use zbus::Connection;
use zbus::Proxy;
use zbus::zvariant::OwnedObjectPath;

const ATSPI_ACCESSIBLE: &str = "org.a11y.atspi.Accessible";

#[derive(Parser)]
#[command(name = "tvpilot", about = "AT-SPI automation for Tizen TV")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Take a snapshot of the active accessibility tree
    Snap {
        /// Show only interactive elements (sensitive/focusable/highlightable)
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Verbose per-element output (role, name, state flags)
        #[arg(short = 'v', long)]
        verbose: bool,
        /// Suppress tree output — useful for timing the walk only
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Target a single app by bus name (e.g. :1.55). Bypasses
        /// active-window detection.
        #[arg(short = 'a', long)]
        app: Option<String>,
        /// Walk every registered app instead of just the active one.
        /// Default: detect the app whose root has STATE_ACTIVE and walk only
        /// that one (matches what aurum-cli dump-tree shows you).
        #[arg(long)]
        all: bool,
        /// AT-SPI bus address. Defaults to /run/user/5001/at-spi/bus
        #[arg(long)]
        bus: Option<String>,
    },
}

// AT-SPI state bit indices (atspi-constants.h). The wire format is `au` —
// two-word bitmask: bits 0-31 in raw[0], bits 32-63 in raw[1].
const STATE_ACTIVE: u32 = 1;
const STATE_FOCUSABLE: u32 = 11;
const STATE_FOCUSED: u32 = 12;
const STATE_SENSITIVE: u32 = 24;
const STATE_SHOWING: u32 = 25;
const STATE_VISIBLE: u32 = 30;
// Samsung Tizen extensions.
const STATE_HIGHLIGHTED: u32 = 39;
const STATE_HIGHLIGHTABLE: u32 = 41;

#[derive(Default, Copy, Clone)]
struct StateBits {
    raw: [u32; 2],
}
impl StateBits {
    fn from_vec(v: Vec<u32>) -> Self {
        let raw = match v.len() {
            0 => [0, 0],
            1 => [v[0], 0],
            _ => [v[0], v[1]],
        };
        Self { raw }
    }
    fn has(&self, idx: u32) -> bool {
        if idx < 32 {
            (self.raw[0] >> idx) & 1 == 1
        } else if idx < 64 {
            (self.raw[1] >> (idx - 32)) & 1 == 1
        } else {
            false
        }
    }
    fn active(&self) -> bool { self.has(STATE_ACTIVE) }
    fn focusable(&self) -> bool { self.has(STATE_FOCUSABLE) }
    fn focused(&self) -> bool { self.has(STATE_FOCUSED) }
    fn sensitive(&self) -> bool { self.has(STATE_SENSITIVE) }
    fn showing(&self) -> bool { self.has(STATE_SHOWING) }
    fn visible(&self) -> bool { self.has(STATE_VISIBLE) }
    fn highlightable(&self) -> bool { self.has(STATE_HIGHLIGHTABLE) }
    fn highlighted(&self) -> bool { self.has(STATE_HIGHLIGHTED) }
    fn is_interactive(&self) -> bool {
        (self.sensitive() || self.focusable() || self.highlightable())
            && self.visible()
            && self.showing()
    }
    fn short_flags(&self) -> String {
        let mut s = String::new();
        if self.focusable()    { s.push('f'); }
        if self.focused()      { s.push('F'); }
        if self.sensitive()    { s.push('s'); }
        if self.visible()      { s.push('v'); }
        if self.showing()      { s.push('S'); }
        if self.highlightable(){ s.push('h'); }
        if self.highlighted()  { s.push('H'); }
        s
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Snap { interactive, verbose, quiet, app, all, bus } => {
            snap(interactive, verbose, quiet, app, all, bus).await
        }
    }
}

async fn snap(
    interactive: bool,
    verbose: bool,
    quiet: bool,
    app_filter: Option<String>,
    walk_all: bool,
    bus: Option<String>,
) -> Result<()> {
    let atspi_addr = bus.unwrap_or_else(default_atspi_addr);
    eprintln!("[tvpilot] at-spi bus: {}", atspi_addr);

    let t = Instant::now();
    let atspi = zbus::connection::Builder::address(atspi_addr.as_str())?
        .build()
        .await
        .context("connect to at-spi bus")?;
    eprintln!("[tvpilot] connect: {:?}", t.elapsed());

    let t = Instant::now();
    let mut apps = list_apps(&atspi).await?;
    eprintln!("[tvpilot] list_apps: {:?} ({} apps)", t.elapsed(), apps.len());

    if let Some(target) = &app_filter {
        apps.retain(|(s, _)| s == target);
        eprintln!("[tvpilot] filtered to --app {} ({} match)", target, apps.len());
    } else if !walk_all {
        let t = Instant::now();
        let active = find_active_app(&atspi, &apps).await;
        eprintln!("[tvpilot] active-app detect: {:?}", t.elapsed());
        match active {
            Some(a) => {
                eprintln!("[tvpilot] active app: {}", a.0);
                apps.retain(|(s, _)| s == &a.0);
            }
            None => {
                eprintln!(
                    "[tvpilot] no app has STATE_ACTIVE — falling back to all apps. Pass --all to silence."
                );
            }
        }
    }
    for (sender, path) in &apps {
        eprintln!("[tvpilot]   {} {}", sender, path);
    }

    if apps.is_empty() {
        return Err(anyhow!("no a11y apps to walk — is aurum-bootstrap running, or did --app match nothing?"));
    }

    // Walk every app concurrently. Each app's walk also internally fans out
    // siblings concurrently. Output is collected per-app into a String so the
    // tree shape is preserved even with parallel traversal.
    let t_total = Instant::now();
    let app_walks: Vec<_> = apps
        .iter()
        .map(|(s, p)| walk_app(atspi.clone(), s.clone(), p.clone(), interactive, verbose))
        .collect();
    let results = futures::future::join_all(app_walks).await;
    let total_elapsed = t_total.elapsed();

    let mut total_nodes = 0;
    for (i, r) in results.into_iter().enumerate() {
        let (sender, path) = &apps[i];
        match r {
            Ok((nodes, elapsed, out)) => {
                total_nodes += nodes;
                eprintln!(
                    "[tvpilot] {} {} nodes in {:?}",
                    sender, nodes, elapsed
                );
                if !quiet {
                    println!("# app={} path={}", sender, path);
                    print!("{}", out);
                }
            }
            Err(e) => eprintln!("[tvpilot] {} failed: {:#}", sender, e),
        }
    }

    eprintln!(
        "[tvpilot] TOTAL: {} nodes across {} apps in {:?}",
        total_nodes, apps.len(), total_elapsed
    );
    Ok(())
}

fn default_atspi_addr() -> String {
    "unix:path=/run/user/5001/at-spi/bus".to_string()
}

async fn list_apps(atspi: &Connection) -> Result<Vec<(String, String)>> {
    let proxy = Proxy::new(
        atspi,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        ATSPI_ACCESSIBLE,
    )
    .await?;
    let children: Vec<(String, OwnedObjectPath)> = proxy
        .call("GetChildren", &())
        .await
        .context("Registry.GetChildren")?;
    Ok(children
        .into_iter()
        .map(|(s, p)| (s, p.to_string()))
        .collect())
}

/// Query GetState on every app's accessible root concurrently and return the
/// first one whose root has STATE_ACTIVE. Confirmed on Tizen 10: only the
/// top-level foreground app sets this bit.
async fn find_active_app(
    atspi: &Connection,
    apps: &[(String, String)],
) -> Option<(String, String)> {
    let probes = apps.iter().map(|(s, p)| {
        let conn = atspi.clone();
        let sender = s.clone();
        let path = p.clone();
        async move {
            let proxy = Proxy::new(&conn, sender.as_str(), path.as_str(), ATSPI_ACCESSIBLE)
                .await
                .ok()?;
            let state: Vec<u32> = proxy.call("GetState", &()).await.ok()?;
            let bits = StateBits::from_vec(state);
            if bits.active() {
                Some((sender, path))
            } else {
                None
            }
        }
    });
    let results = futures::future::join_all(probes).await;
    results.into_iter().flatten().next()
}

async fn walk_app(
    atspi: Connection,
    sender: String,
    path: String,
    interactive: bool,
    verbose: bool,
) -> Result<(usize, std::time::Duration, String)> {
    let t = Instant::now();
    let (count, out) = walk_node(atspi, sender, path, 0, interactive, verbose).await;
    Ok((count, t.elapsed(), out))
}

/// Recursive async walk. For each node, fan out the 4 D-Bus calls
/// (`GetRoleName`, `Name`, `GetState`, `GetChildren`) concurrently with
/// `tokio::join!`, then walk all children concurrently with `join_all`.
/// Returns the buffered output for this subtree so tree shape is preserved
/// even with parallel traversal. Subtrees whose state lacks `SHOWING` are
/// not walked (visibility-pruning).
fn walk_node(
    atspi: Connection,
    sender: String,
    path: String,
    indent: usize,
    interactive: bool,
    verbose: bool,
) -> BoxFuture<'static, (usize, String)> {
    async move {
        let proxy = match Proxy::new(&atspi, sender.as_str(), path.as_str(), ATSPI_ACCESSIBLE).await
        {
            Ok(p) => p,
            Err(_) => return (0, String::new()),
        };

        // Fan out the per-node fetches. zbus multiplexes them on the single
        // connection by serial number; the server processes them however it
        // can. Even if server-side is serialized, we save the per-call
        // round-trip stalls.
        let role_f = proxy.call::<_, _, String>("GetRoleName", &());
        let name_f = proxy.get_property::<String>("Name");
        let state_f = proxy.call::<_, _, Vec<u32>>("GetState", &());
        let children_f = proxy.call::<_, _, Vec<(String, OwnedObjectPath)>>("GetChildren", &());

        let (role, name, state, children) = tokio::join!(role_f, name_f, state_f, children_f);

        let role = role.unwrap_or_else(|_| "?".to_string());
        let name = name.unwrap_or_default();
        let state = StateBits::from_vec(state.unwrap_or_default());
        let children = children.unwrap_or_default();

        let mut out = String::new();
        let mut count = 0;

        // Prune non-showing subtrees entirely.
        if !state.showing() {
            return (0, out);
        }

        let show = !interactive || state.is_interactive();
        if show {
            let pad = " ".repeat(indent * 2);
            let marker = if state.focused() { "*" } else { "" };
            let line_name = if name.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", name)
            };
            if verbose {
                out.push_str(&format!(
                    "{}- {}{}{}  [{}]\n",
                    pad,
                    role,
                    marker,
                    line_name,
                    state.short_flags()
                ));
            } else {
                out.push_str(&format!("{}- {}{}{}\n", pad, role, marker, line_name));
            }
            count += 1;
        }

        let next_indent = if show { indent + 1 } else { indent };
        let child_futs: Vec<_> = children
            .into_iter()
            .map(|(cs, cp)| {
                walk_node(
                    atspi.clone(),
                    cs,
                    cp.to_string(),
                    next_indent,
                    interactive,
                    verbose,
                )
            })
            .collect();
        let child_results = futures::future::join_all(child_futs).await;
        for (c_count, c_out) in child_results {
            count += c_count;
            out.push_str(&c_out);
        }

        (count, out)
    }
    .boxed()
}
