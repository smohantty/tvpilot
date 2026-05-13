//! AT-SPI client via the `dbus` crate. Async pipelined walk with concurrent
//! per-node fetches, then a single sequential render pass that assigns
//! monotonic `eN` refs in tree-traversal order (per PLAN.md "Ref design").

use anyhow::{Context, Result};
use dbus::arg::Variant;
use dbus::channel::Channel;
use dbus::message::{MatchRule, MessageType};
use dbus::nonblock::{Proxy, SyncConnection};
use futures::future::{BoxFuture, FutureExt};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Pointer to the element AT-SPI most recently flagged as
/// focused/highlighted. Updated by the signal listener task, read by render
/// to overlay the `*` marker even when `GetState` doesn't reflect it.
pub type FocusPointer = Arc<Mutex<Option<(String, String)>>>;

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
    pub fn highlighted(&self) -> bool { self.has(STATE_HIGHLIGHTED) }
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

/// Subscribe to `org.a11y.atspi.Event.Object` StateChanged signals on the
/// AT-SPI bus and keep `last_focus` pointing at whichever element most
/// recently set its `focused` or `highlighted` state to true. This lets the
/// agent track the TV-nav cursor even when `GetState` on the element no
/// longer reflects it (a Tizen Dali quirk — events fire but state polls
/// don't show the bit).
pub async fn install_focus_listener(
    conn: &Arc<SyncConnection>,
    last_focus: FocusPointer,
) -> Result<()> {
    // Tell the AT-SPI bus daemon to forward signals matching our rule.
    let dbus_proxy = Proxy::new(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        TIMEOUT,
        conn.clone(),
    );
    let rule_str =
        "type='signal',interface='org.a11y.atspi.Event.Object',member='StateChanged'";
    dbus_proxy
        .method_call::<(), _, _, _>("org.freedesktop.DBus", "AddMatch", (rule_str,))
        .await
        .context("AddMatch StateChanged")?;

    let mr = MatchRule::new()
        .with_type(MessageType::Signal)
        .with_interface("org.a11y.atspi.Event.Object")
        .with_member("StateChanged");

    use dbus::channel::MatchingReceiver;
    conn.start_receive(
        mr,
        Box::new(move |msg, _| {
            // AT-SPI StateChanged body: (siiv(so))
            //   detail  = state name e.g. "focused", "highlighted"
            //   detail1 = 1 (set) or 0 (cleared)
            let mut it = msg.iter_init();
            let detail: String = match it.read() {
                Ok(s) => s,
                Err(_) => return true,
            };
            let value: i32 = it.read().unwrap_or(0);
            if value != 1 {
                return true; // only "becomes-set"
            }
            if detail != "focused" && detail != "highlighted" {
                return true;
            }
            let sender = match msg.sender() {
                Some(s) => s.to_string(),
                None => return true,
            };
            let path = match msg.path() {
                Some(p) => p.to_string(),
                None => return true,
            };
            *last_focus.lock().unwrap() = Some((sender, path));
            true
        }),
    );
    Ok(())
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

/// Intermediate tree built from the concurrent walk. Refs are NOT yet
/// assigned — that happens deterministically in a sequential render pass so
/// `eN` ids match tree-traversal order regardless of which `tokio::join!`
/// future finished first.
pub struct Node {
    pub sender: String,
    pub path: String,
    pub role: String,
    pub name: String,
    pub description: String,
    pub state: StateBits,
    pub children: Vec<Node>,
}

/// RefMap entry, populated during render. Carries enough to act on the
/// element later (click ladder, etc.) — `(bus_name, object_path, role, name)`
/// per PLAN.md "Ref design".
#[derive(Clone)]
pub struct RefEntry {
    pub sender: String,
    pub path: String,
    pub role: String,
    pub name: String,
}

/// Read the AT-SPI state bits for one element.
pub async fn get_state(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
) -> Result<StateBits> {
    let proxy = Proxy::new(sender.to_string(), path.to_string(), TIMEOUT, conn.clone());
    let (v,): (Vec<u32>,) = proxy
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
    let proxy = Proxy::new(sender.to_string(), path.to_string(), TIMEOUT, conn.clone());
    // GetActions returns a(sss) — array of (name, description, key_binding).
    let (actions,): (Vec<(String, String, String)>,) = proxy
        .method_call("org.a11y.atspi.Action", "GetActions", ())
        .await
        .ok()?;
    for (i, (name, _, _)) in actions.iter().enumerate() {
        let n = name.to_ascii_lowercase();
        if n == "click" || n == "activate" || n == "default" {
            return Some(i as i32);
        }
    }
    None
}

/// Invoke `Action.DoAction(idx)` on the element.
pub async fn do_action(
    conn: &Arc<SyncConnection>,
    sender: &str,
    path: &str,
    idx: i32,
) -> Result<bool> {
    let proxy = Proxy::new(sender.to_string(), path.to_string(), TIMEOUT, conn.clone());
    let (ok,): (bool,) = proxy
        .method_call("org.a11y.atspi.Action", "DoAction", (idx,))
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
    let proxy = Proxy::new(sender.to_string(), path.to_string(), TIMEOUT, conn.clone());
    let (ok,): (bool,) = proxy
        .method_call("org.a11y.atspi.Component", "GrabHighlight", ())
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
    let proxy = Proxy::new(sender.to_string(), path.to_string(), TIMEOUT, conn.clone());
    let (ok,): (bool,) = proxy
        .method_call("org.a11y.atspi.Component", "GrabFocus", ())
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
    focus: Option<(String, String)>,
) -> (String, u32, Vec<RefEntry>) {
    let root = build_tree(conn, sender, path).await;
    let mut refmap: Vec<RefEntry> = Vec::new();
    let mut out = String::new();
    let mut total: u32 = 0;
    render(
        &root,
        0,
        &mut refmap,
        &mut out,
        &mut total,
        interactive_only,
        verbose,
        focus.as_ref(),
    );
    (out, total, refmap)
}

fn build_tree(
    conn: Arc<SyncConnection>,
    sender: String,
    path: String,
) -> BoxFuture<'static, Node> {
    async move {
        let proxy = Proxy::new(sender.clone(), path.clone(), TIMEOUT, conn.clone());

        // 5 calls per node — all fan out concurrently. Description is often
        // the user-visible label for Dali widgets where Name is an internal
        // identifier; we surface whichever reads as more meaningful at render.
        let role_f = proxy.method_call::<(String,), _, _, _>(ATSPI_ACCESSIBLE, "GetRoleName", ());
        let name_f = proxy.method_call::<(Variant<String>,), _, _, _>(
            ATSPI_PROPS,
            "Get",
            (ATSPI_ACCESSIBLE, "Name"),
        );
        let desc_f = proxy.method_call::<(Variant<String>,), _, _, _>(
            ATSPI_PROPS,
            "Get",
            (ATSPI_ACCESSIBLE, "Description"),
        );
        let state_f = proxy.method_call::<(Vec<u32>,), _, _, _>(ATSPI_ACCESSIBLE, "GetState", ());
        let children_f = proxy.method_call::<(Vec<(String, dbus::Path<'static>)>,), _, _, _>(
            ATSPI_ACCESSIBLE,
            "GetChildren",
            (),
        );

        let (role, name, desc, state, children) =
            tokio::join!(role_f, name_f, desc_f, state_f, children_f);
        let role = role.map(|(s,)| s).unwrap_or_else(|_| "?".to_string());
        let name = name.map(|(Variant(s),)| s).unwrap_or_default();
        let description = desc.map(|(Variant(s),)| s).unwrap_or_default();
        let state = StateBits::from_vec(state.map(|(v,)| v).unwrap_or_default());
        let children_pairs = children.map(|(v,)| v).unwrap_or_default();

        if !state.showing() {
            return Node {
                sender,
                path,
                role,
                name,
                description,
                state,
                children: Vec::new(),
            };
        }

        let child_futs: Vec<_> = children_pairs
            .into_iter()
            .map(|(cs, cp)| build_tree(conn.clone(), cs, cp.to_string()))
            .collect();
        let children = futures::future::join_all(child_futs).await;

        Node {
            sender,
            path,
            role,
            name,
            description,
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
    let has_label = !node.name.is_empty() || !node.description.is_empty();
    match role_tier(&node.role) {
        Tier::Interactive => true,
        Tier::Content => has_label,
        Tier::Structural => false,
        Tier::Unknown => node.state.is_interactive() || has_label,
    }
}

/// Pick the most human-readable label for this node. On Tizen Dali widgets,
/// `Name` is usually an internal identifier ("d/ug.content.category@…");
/// `Description` is often the user-visible label. We prefer Description when
/// it's non-empty AND distinct from Name (some widgets duplicate them);
/// otherwise fall back to Name.
fn label_of(node: &Node) -> &str {
    if !node.description.is_empty() && node.description != node.name {
        node.description.as_str()
    } else {
        node.name.as_str()
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
    focus: Option<&(String, String)>,
) {
    if !node.state.showing() {
        return;
    }

    let bears = ref_bearing(node);
    let surface = bears && (!interactive_only || node.state.is_interactive());

    let next_indent = if surface {
        let ref_idx = refmap.len() + 1;
        let ref_id = format!("e{}", ref_idx);
        let label = label_of(node).to_string();
        refmap.push(RefEntry {
            sender: node.sender.clone(),
            path: node.path.clone(),
            role: node.role.clone(),
            name: label.clone(),
        });

        let pad = " ".repeat(indent * 2);
        // Mark focused on either the live state bits (works on standard
        // a11y) or the event-tracked focus pointer (works on Tizen Dali,
        // where state bits stay 0 but events fire).
        let focused_by_state = node.state.focused() || node.state.highlighted();
        let focused_by_event = focus
            .map(|(s, p)| s == &node.sender && p == &node.path)
            .unwrap_or(false);
        let focused_now = focused_by_state || focused_by_event;
        let marker = if focused_now { "*" } else { "" };
        let label_part = if label.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"{}", label, marker)
        };
        if verbose {
            let flags_part = format!(" [{}]", node.state.short_flags());
            out.push_str(&format!(
                "{}- {}{}{} [ref={}]\n",
                pad, node.role, label_part, flags_part, ref_id
            ));
        } else if label.is_empty() {
            // No label at all: surface the role so the line says something.
            out.push_str(&format!("{}- {}{} [ref={}]\n", pad, node.role, marker, ref_id));
        } else {
            out.push_str(&format!("{}-{} [ref={}]\n", pad, label_part, ref_id));
        }
        *total += 1;
        indent + 1
    } else {
        indent
    };

    for child in &node.children {
        render(
            child,
            next_indent,
            refmap,
            out,
            total,
            interactive_only,
            verbose,
            focus,
        );
    }
}
