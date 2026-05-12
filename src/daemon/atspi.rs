//! AT-SPI client via the `dbus` crate. Async pipelined walk with concurrent
//! per-node fetches.

use anyhow::{Context, Result};
use dbus::arg::Variant;
use dbus::channel::Channel;
use dbus::nonblock::{Proxy, SyncConnection};
use futures::future::{BoxFuture, FutureExt};
use std::sync::Arc;
use std::time::Duration;

const ATSPI_ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ATSPI_PROPS: &str = "org.freedesktop.DBus.Properties";
const TIMEOUT: Duration = Duration::from_secs(5);

// AT-SPI state bit indices (atspi-constants.h).
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
pub struct StateBits {
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
    pub fn active(&self) -> bool { self.has(STATE_ACTIVE) }
    pub fn focused(&self) -> bool { self.has(STATE_FOCUSED) }
    pub fn showing(&self) -> bool { self.has(STATE_SHOWING) }
    pub fn is_interactive(&self) -> bool {
        (self.has(STATE_SENSITIVE) || self.has(STATE_FOCUSABLE) || self.has(STATE_HIGHLIGHTABLE))
            && self.has(STATE_VISIBLE)
            && self.has(STATE_SHOWING)
    }
    pub fn short_flags(&self) -> String {
        let mut s = String::new();
        if self.has(STATE_FOCUSABLE)     { s.push('f'); }
        if self.has(STATE_FOCUSED)       { s.push('F'); }
        if self.has(STATE_SENSITIVE)     { s.push('s'); }
        if self.has(STATE_VISIBLE)       { s.push('v'); }
        if self.has(STATE_SHOWING)       { s.push('S'); }
        if self.has(STATE_HIGHLIGHTABLE) { s.push('h'); }
        if self.has(STATE_HIGHLIGHTED)   { s.push('H'); }
        s
    }
}

/// Connect to the AT-SPI bus (plain unix socket on Tizen).
pub async fn connect_atspi() -> Result<Arc<SyncConnection>> {
    let addr = "unix:path=/run/user/5001/at-spi/bus";
    let mut channel = Channel::open_private(addr).context("open at-spi channel")?;
    channel.register().context("register at-spi")?;
    let (resource, conn) = dbus_tokio::connection::from_channel::<SyncConnection>(channel)
        .context("dbus_tokio from_channel")?;
    tokio::spawn(async move {
        let err = resource.await;
        eprintln!("[tvpilotd] at-spi resource ended: {}", err);
    });
    Ok(conn)
}

/// Toggle `org.a11y.Status.IsEnabled = true` on the session bus. On Tizen 10
/// this is kdbus; libdbus-1 handles it transparently via `new_session_sync`.
pub async fn enable_a11y() -> Result<()> {
    let (resource, conn) =
        dbus_tokio::connection::new_session_sync().context("session bus connect")?;
    tokio::spawn(async move {
        let _ = resource.await;
    });
    let bus = Proxy::new("org.a11y.Bus", "/org/a11y/bus", TIMEOUT, conn);
    let v: Variant<bool> = Variant(true);
    bus.method_call::<(), _, _, _>(
        ATSPI_PROPS,
        "Set",
        ("org.a11y.Status", "IsEnabled", v),
    )
    .await
    .context("Properties.Set IsEnabled")?;
    Ok(())
}

/// List every app registered on the AT-SPI bus.
pub async fn list_apps(conn: &Arc<SyncConnection>) -> Result<Vec<(String, String)>> {
    let proxy = Proxy::new(
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        TIMEOUT,
        conn.clone(),
    );
    let (children,): (Vec<(String, dbus::Path<'static>)>,) = proxy
        .method_call(ATSPI_ACCESSIBLE, "GetChildren", ())
        .await
        .context("Registry.GetChildren")?;
    Ok(children
        .into_iter()
        .map(|(s, p)| (s, p.to_string()))
        .collect())
}

/// Probe every app's root state in parallel and return the one with
/// `STATE_ACTIVE` set. Confirmed on Tizen 10: only the foreground app sets it.
pub async fn find_active_app(
    conn: &Arc<SyncConnection>,
    apps: &[(String, String)],
) -> Option<(String, String)> {
    let probes = apps.iter().map(|(s, p)| {
        let c = conn.clone();
        let s = s.clone();
        let p = p.clone();
        async move {
            let pr = Proxy::new(s.clone(), p.clone(), TIMEOUT, c);
            let (state,): (Vec<u32>,) = pr
                .method_call(ATSPI_ACCESSIBLE, "GetState", ())
                .await
                .ok()?;
            if StateBits::from_vec(state).active() {
                Some((s, p))
            } else {
                None
            }
        }
    });
    let results = futures::future::join_all(probes).await;
    results.into_iter().flatten().next()
}

/// Recursive async walk. Per node we issue 4 D-Bus calls concurrently
/// (`GetRoleName`, `Name` property, `GetState`, `GetChildren`), then recurse
/// into all children concurrently. Subtrees lacking `SHOWING` are pruned.
/// Returns (rendered tree text, node count).
pub async fn walk(
    conn: Arc<SyncConnection>,
    sender: String,
    path: String,
    interactive_only: bool,
    verbose: bool,
) -> (String, u32) {
    let (out, count) = walk_node(conn, sender, path, 0, interactive_only, verbose).await;
    (out, count)
}

fn walk_node(
    conn: Arc<SyncConnection>,
    sender: String,
    path: String,
    indent: usize,
    interactive_only: bool,
    verbose: bool,
) -> BoxFuture<'static, (String, u32)> {
    async move {
        let proxy = Proxy::new(
            sender.clone(),
            path.clone(),
            TIMEOUT,
            conn.clone(),
        );

        let role_f = proxy.method_call::<(String,), _, _, _>(ATSPI_ACCESSIBLE, "GetRoleName", ());
        let name_f = proxy.method_call::<(Variant<String>,), _, _, _>(
            ATSPI_PROPS,
            "Get",
            (ATSPI_ACCESSIBLE, "Name"),
        );
        let state_f = proxy.method_call::<(Vec<u32>,), _, _, _>(ATSPI_ACCESSIBLE, "GetState", ());
        let children_f = proxy.method_call::<(Vec<(String, dbus::Path<'static>)>,), _, _, _>(
            ATSPI_ACCESSIBLE,
            "GetChildren",
            (),
        );

        let (role, name, state, children) = tokio::join!(role_f, name_f, state_f, children_f);

        let role = role.map(|(s,)| s).unwrap_or_else(|_| "?".to_string());
        let name = name.map(|(Variant(s),)| s).unwrap_or_default();
        let state = StateBits::from_vec(state.map(|(v,)| v).unwrap_or_default());
        let children = children.map(|(v,)| v).unwrap_or_default();

        // Visibility prune.
        if !state.showing() {
            return (String::new(), 0);
        }

        let mut out = String::new();
        let mut count = 0;
        let show = !interactive_only || state.is_interactive();
        if show {
            let pad = " ".repeat(indent * 2);
            let marker = if state.focused() { "*" } else { "" };
            let name_part = if name.is_empty() {
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
                    name_part,
                    state.short_flags()
                ));
            } else {
                out.push_str(&format!("{}- {}{}{}\n", pad, role, marker, name_part));
            }
            count = 1;
        }

        let next_indent = if show { indent + 1 } else { indent };
        let child_futs: Vec<_> = children
            .into_iter()
            .map(|(cs, cp)| {
                walk_node(
                    conn.clone(),
                    cs,
                    cp.to_string(),
                    next_indent,
                    interactive_only,
                    verbose,
                )
            })
            .collect();
        let child_results = futures::future::join_all(child_futs).await;
        for (c_out, c_count) in child_results {
            out.push_str(&c_out);
            count += c_count;
        }

        (out, count)
    }
    .boxed()
}
