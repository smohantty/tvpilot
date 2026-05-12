//! AT-SPI client via the `dbus` crate. Async pipelined walk with concurrent
//! per-node fetches, then a single sequential render pass that assigns
//! monotonic `eN` refs in tree-traversal order (per PLAN.md "Ref design").

use anyhow::{Context, Result};
use dbus::arg::Variant;
use dbus::channel::Channel;
use dbus::nonblock::{Proxy, SyncConnection};
use futures::future::{BoxFuture, FutureExt};
use std::sync::Arc;
use std::time::Duration;

// ─── role tiers, per PLAN.md "Role classification" ──────────────────────────
const INTERACTIVE_ROLES: &[&str] = &[
    "push button",
    "menu item",
    "menu item radio",
    "menu item check",
    "check box",
    "radio button",
    "toggle button",
    "slider",
    "entry",
    "combo box",
    "list item",
    "tab",
    "tree item",
    // Tizen / extra variants
    "button",
    "link",
    "scroll bar",
];

const CONTENT_ROLES: &[&str] = &[
    "heading",
    "label",
    "image",
    "tooltip",
    "table cell",
    "column header",
    "row header",
    "static",
    "text",
];

const STRUCTURAL_ROLES: &[&str] = &[
    "filler",
    "redundant object",
    "panel",
    "container",
    "scroll pane",
    "viewport",
    "section",
    "page tab list",
    "window",
    "application",
    "frame",
    "internal frame",
    "layered pane",
    "split pane",
    "form",
];

#[derive(Copy, Clone, Debug)]
enum Tier {
    Interactive,
    Content,
    Structural,
    Unknown,
}

fn role_tier(role: &str) -> Tier {
    if INTERACTIVE_ROLES.contains(&role) {
        Tier::Interactive
    } else if CONTENT_ROLES.contains(&role) {
        Tier::Content
    } else if STRUCTURAL_ROLES.contains(&role) {
        Tier::Structural
    } else {
        Tier::Unknown
    }
}

const ATSPI_ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ATSPI_ACTION: &str = "org.a11y.atspi.Action";
const ATSPI_COMPONENT: &str = "org.a11y.atspi.Component";
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
        let mut raw = [0u32; 2];
        for (slot, bits) in raw.iter_mut().zip(v) {
            *slot = bits;
        }
        Self { raw }
    }

    fn has(&self, idx: u32) -> bool {
        match idx {
            0..32 => (self.raw[0] >> idx) & 1 == 1,
            32..64 => (self.raw[1] >> (idx - 32)) & 1 == 1,
            _ => false,
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
        for (bit, ch) in [
            (STATE_FOCUSABLE, 'f'),
            (STATE_FOCUSED, 'F'),
            (STATE_SENSITIVE, 's'),
            (STATE_VISIBLE, 'v'),
            (STATE_SHOWING, 'S'),
            (STATE_HIGHLIGHTABLE, 'h'),
            (STATE_HIGHLIGHTED, 'H'),
        ] {
            if self.has(bit) {
                s.push(ch);
            }
        }
        s
    }
}

/// Borrowed-string proxy constructor. `Proxy::method_call` builds a fully-owned
/// `Message` synchronously and returns a future that does NOT borrow from the
/// proxy, so we can hand it `&str` and let the proxy drop right after the call.
fn proxy<'a>(
    conn: &Arc<SyncConnection>,
    sender: &'a str,
    path: &'a str,
) -> Proxy<'a, Arc<SyncConnection>> {
    Proxy::new(sender, path, TIMEOUT, conn.clone())
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
        eprintln!("[tvpilotd] at-spi resource ended: {err}");
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
    bus.method_call::<(), _, _, _>(
        ATSPI_PROPS,
        "Set",
        ("org.a11y.Status", "IsEnabled", Variant(true)),
    )
    .await
    .context("Properties.Set IsEnabled")?;
    Ok(())
}

/// List every app registered on the AT-SPI bus.
pub async fn list_apps(conn: &Arc<SyncConnection>) -> Result<Vec<(String, String)>> {
    let proxy = proxy(conn, "org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root");
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
    // Borrow the bus name + path into the probe; clone only on the (rare)
    // success. The probe future doesn't borrow from the proxy, so the strings
    // only need to live across the single method_call dispatch.
    let probes = apps.iter().map(|(s, p)| {
        let conn = conn.clone();
        async move {
            let (state,): (Vec<u32>,) = proxy(&conn, s, p)
                .method_call(ATSPI_ACCESSIBLE, "GetState", ())
                .await
                .ok()?;
            StateBits::from_vec(state)
                .active()
                .then(|| (s.clone(), p.clone()))
        }
    });
    futures::future::join_all(probes).await.into_iter().flatten().next()
}

/// Intermediate tree built from the concurrent walk. Refs are NOT yet
/// assigned — that happens deterministically in a sequential render pass so
/// `eN` ids match tree-traversal order regardless of which `tokio::join!`
/// future finished first.
///
/// `sender` is interned as `Arc<str>` once per app and shared with every
/// descendant; `path` is interned per node. Cloning into the refmap is then
/// a refcount bump rather than a heap allocation.
pub struct Node {
    pub sender: Arc<str>,
    pub path: Arc<str>,
    pub role: String,
    pub name: String,
    pub state: StateBits,
    pub children: Vec<Node>,
}

/// RefMap entry, populated during render. Carries enough to act on the
/// element later (click ladder, etc.) — `(bus_name, object_path, role, name)`
/// per PLAN.md "Ref design".
#[derive(Clone)]
pub struct RefEntry {
    pub sender: Arc<str>,
    pub path: Arc<str>,
    pub role: String,
    pub name: String,
}

/// Read the AT-SPI state bits for one element.
pub async fn get_state(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
) -> Result<StateBits> {
    let (v,): (Vec<u32>,) = proxy(conn, sender, path)
        .method_call(ATSPI_ACCESSIBLE, "GetState", ())
        .await
        .context("GetState")?;
    Ok(StateBits::from_vec(v))
}

/// Probe `Action.GetActions` and look for one named "click" or "activate".
/// Returns its index for `DoAction(idx)`.
pub async fn find_action(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
) -> Option<i32> {
    // GetActions returns a(sss) — array of (name, description, key_binding).
    let (actions,): (Vec<(String, String, String)>,) = proxy(conn, sender, path)
        .method_call(ATSPI_ACTION, "GetActions", ())
        .await
        .ok()?;
    actions.iter().enumerate().find_map(|(i, (name, _, _))| {
        matches!(name.to_ascii_lowercase().as_str(), "click" | "activate" | "default")
            .then_some(i as i32)
    })
}

/// Invoke `Action.DoAction(idx)` on the element.
pub async fn do_action(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
    idx: i32,
) -> Result<bool> {
    let (ok,): (bool,) = proxy(conn, sender, path)
        .method_call(ATSPI_ACTION, "DoAction", (idx,))
        .await
        .context("DoAction")?;
    Ok(ok)
}

/// `Component.GrabHighlight` — Samsung TV-nav focus (visible highlight cursor).
pub async fn grab_highlight(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
) -> Result<bool> {
    let (ok,): (bool,) = proxy(conn, sender, path)
        .method_call(ATSPI_COMPONENT, "GrabHighlight", ())
        .await
        .context("GrabHighlight")?;
    Ok(ok)
}

/// `Component.GrabFocus` — input-focus fallback when highlight isn't supported.
pub async fn grab_focus(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
) -> Result<bool> {
    let (ok,): (bool,) = proxy(conn, sender, path)
        .method_call(ATSPI_COMPONENT, "GrabFocus", ())
        .await
        .context("GrabFocus")?;
    Ok(ok)
}

/// Phase 1: concurrent walk → in-memory `Node` tree.
/// Phase 2: sequential render with monotonic `eN` ref assignment.
pub async fn walk(
    conn: Arc<SyncConnection>,
    sender: String,
    path: String,
    interactive_only: bool,
    verbose: bool,
) -> (String, u32, Vec<RefEntry>) {
    // Intern at the walk root; every descendant either reuses the same
    // `Arc<str>` (same bus, common case) or allocates a fresh one (rare
    // cross-app proxy children).
    let sender: Arc<str> = Arc::from(sender);
    let path: Arc<str> = Arc::from(path);
    let root = build_tree(conn, sender, path).await;
    let mut refmap = Vec::new();
    let mut out = String::new();
    let mut total: u32 = 0;
    render(&root, 0, &mut refmap, &mut out, &mut total, interactive_only, verbose);
    (out, total, refmap)
}

fn build_tree(
    conn: Arc<SyncConnection>,
    sender: Arc<str>,
    path: Arc<str>,
) -> BoxFuture<'static, Node> {
    async move {
        let pr = proxy(&conn, &sender, &path);

        let role_f = pr.method_call::<(String,), _, _, _>(ATSPI_ACCESSIBLE, "GetRoleName", ());
        let name_f = pr.method_call::<(Variant<String>,), _, _, _>(
            ATSPI_PROPS,
            "Get",
            (ATSPI_ACCESSIBLE, "Name"),
        );
        let state_f = pr.method_call::<(Vec<u32>,), _, _, _>(ATSPI_ACCESSIBLE, "GetState", ());
        let children_f = pr.method_call::<(Vec<(String, dbus::Path<'static>)>,), _, _, _>(
            ATSPI_ACCESSIBLE,
            "GetChildren",
            (),
        );

        let (role, name, state, children) = tokio::join!(role_f, name_f, state_f, children_f);
        let role = role.map(|(s,)| s).unwrap_or_else(|_| "?".into());
        let name = name.map(|(Variant(s),)| s).unwrap_or_default();
        let state = StateBits::from_vec(state.map(|(v,)| v).unwrap_or_default());
        let children_pairs = children.map(|(v,)| v).unwrap_or_default();

        // Visibility prune at fetch time: skip walking !SHOWING subtrees.
        if !state.showing() {
            return Node {
                sender,
                path,
                role,
                name,
                state,
                children: Vec::new(),
            };
        }

        // Reuse the parent's interned bus name when the child reports the
        // same — the common case for an in-app subtree. Cross-app proxy
        // nodes fall through and allocate a fresh `Arc<str>`.
        let child_futs = children_pairs.into_iter().map(|(cs, cp)| {
            let child_sender = if cs.as_str() == sender.as_ref() {
                Arc::clone(&sender)
            } else {
                Arc::<str>::from(cs)
            };
            // `dbus::Path<'static>` derefs to `str`; intern as `Arc<str>`
            // so the later refmap push is a refcount bump.
            let child_path: Arc<str> = Arc::from(&*cp);
            build_tree(conn.clone(), child_sender, child_path)
        });
        let children = futures::future::join_all(child_futs).await;

        Node {
            sender,
            path,
            role,
            name,
            state,
            children,
        }
    }
    .boxed()
}

/// Decide whether a node gets a ref. Per PLAN.md "Role classification":
///   - Interactive role → always
///   - Content role → iff name non-empty
///   - Structural role → never (children float up at the same indent)
///   - Unknown role (Tizen Dali widgets, etc.) → state-based promotion:
///     ref if the node is interactive (sensitive/focusable/highlightable)
///     or has a non-empty name (so labels still surface)
fn ref_bearing(node: &Node) -> bool {
    if !node.state.showing() {
        return false;
    }
    match role_tier(&node.role) {
        Tier::Interactive => true,
        Tier::Content => !node.name.is_empty(),
        Tier::Structural => false,
        Tier::Unknown => node.state.is_interactive() || !node.name.is_empty(),
    }
}

fn render(
    node: &Node,
    indent: usize,
    refmap: &mut Vec<RefEntry>,
    out: &mut String,
    total: &mut u32,
    interactive_only: bool,
    verbose: bool,
) {
    use std::fmt::Write as _;

    if !node.state.showing() {
        return;
    }

    let bears = ref_bearing(node);
    let surface = bears && (!interactive_only || node.state.is_interactive());

    let next_indent = if surface {
        let ref_idx = refmap.len() + 1;
        refmap.push(RefEntry {
            sender: Arc::clone(&node.sender),
            path: Arc::clone(&node.path),
            role: node.role.clone(),
            name: node.name.clone(),
        });

        // Stream directly into `out` — no intermediate `pad`/`name_part`
        // String allocations. `write!` on `String` is infallible, so we
        // discard the `Result`.
        for _ in 0..indent {
            out.push_str("  ");
        }
        let marker = if node.state.focused() { "*" } else { "" };

        let _ = if verbose {
            if node.name.is_empty() {
                writeln!(out, "- {} [{}] [ref=e{ref_idx}]", node.role, node.state.short_flags())
            } else {
                writeln!(
                    out,
                    "- {} \"{}\"{marker} [{}] [ref=e{ref_idx}]",
                    node.role,
                    node.name,
                    node.state.short_flags(),
                )
            }
        } else if node.name.is_empty() {
            // No name: surface the role so the agent at least sees
            // *something*. Rare given our ref-bearing rule.
            writeln!(out, "- {} [ref=e{ref_idx}]", node.role)
        } else {
            // Default: minimal — name + focus marker + ref. Role is omitted
            // because on Tizen Dali widgets it is uniformly "unknown" and
            // adds no decision signal for the agent.
            writeln!(out, "- \"{}\"{marker} [ref=e{ref_idx}]", node.name)
        };

        *total += 1;
        indent + 1
    } else {
        // Structural or skipped: children float up at the same indent level.
        indent
    };

    for child in &node.children {
        render(child, next_indent, refmap, out, total, interactive_only, verbose);
    }
}
