//! AT-SPI client via the `dbus` crate. Async pipelined walk with concurrent
//! per-node fetches, then a single sequential render pass that assigns
//! monotonic `eN` refs in tree-traversal order (per PLAN.md "Ref design").

use anyhow::{Context, Result};
use dbus::arg::Variant;
use dbus::channel::Channel;
use dbus::message::{MatchRule, MessageType};
use dbus::nonblock::{Proxy, SyncConnection};
use futures::future::{BoxFuture, FutureExt};
use std::collections::HashMap;
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
const ATSPI_COMPONENT: &str = "org.a11y.atspi.Component";
const ATSPI_PROPS: &str = "org.freedesktop.DBus.Properties";
const TIMEOUT: Duration = Duration::from_secs(5);

/// AT-SPI Component.GetExtents coord types.
const COORD_SCREEN: u32 = 0;

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
///
/// `extents` and `attributes` are not yet consumed by render; we fetch them
/// so the daemon's per-node work matches aurum's fallback path
/// (`updateExtents` + `updateAttributes`) for an apples-to-apples Rust-vs-C++
/// perf comparison. They also land in `tvpilot-raw.txt` for inspection.
pub struct Node {
    pub sender: String,
    pub path: String,
    pub role: String,
    pub name: String,
    pub description: String,
    pub state: StateBits,
    pub extents: Option<(i32, i32, i32, i32)>,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
}

/// RefMap entry, populated during render. Carries enough to act on the
/// element later (click ladder, etc.) — `(bus_name, object_path, role, name)`
/// per PLAN.md "Ref design".
#[derive(Clone)]
#[allow(dead_code)]
pub struct RefEntry {
    pub sender: String,
    pub path: String,
    pub role: String,
    pub name: String,
}

/// Phase 1: concurrent walk → in-memory `Node` tree. Public so callers can
/// dump or transform the raw tree before rendering.
pub fn build_tree(
    conn: Arc<SyncConnection>,
    sender: String,
    path: String,
) -> BoxFuture<'static, Node> {
    async move {
        let proxy = Proxy::new(sender.clone(), path.clone(), TIMEOUT, conn.clone());

        // 7 calls per node, all fanned out concurrently. Matches aurum's
        // fallback path (updateRoleName / updateName+Desc / updateStates /
        // updateExtents / updateAttributes / GetChildren). Description is
        // often the user-visible label on Dali widgets; extents and
        // attributes are kept on the Node so per-node fetch volume is
        // comparable for the Rust-vs-C++ measurement, even when render
        // ignores them.
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
        let extents_f = proxy.method_call::<((i32, i32, i32, i32),), _, _, _>(
            ATSPI_COMPONENT,
            "GetExtents",
            (COORD_SCREEN,),
        );
        let attrs_f = proxy.method_call::<(HashMap<String, String>,), _, _, _>(
            ATSPI_ACCESSIBLE,
            "GetAttributes",
            (),
        );
        let children_f = proxy.method_call::<(Vec<(String, dbus::Path<'static>)>,), _, _, _>(
            ATSPI_ACCESSIBLE,
            "GetChildren",
            (),
        );

        let (role, name, desc, state, extents, attrs, children) = tokio::join!(
            role_f, name_f, desc_f, state_f, extents_f, attrs_f, children_f
        );
        let role = role.map(|(s,)| s).unwrap_or_else(|_| "?".to_string());
        let name = name.map(|(Variant(s),)| s).unwrap_or_default();
        let description = desc.map(|(Variant(s),)| s).unwrap_or_default();
        let state = StateBits::from_vec(state.map(|(v,)| v).unwrap_or_default());
        let extents = extents.map(|(r,)| r).ok();
        let attributes: Vec<(String, String)> = attrs
            .map(|(m,)| {
                let mut v: Vec<(String, String)> = m.into_iter().collect();
                v.sort_by(|a, b| a.0.cmp(&b.0));
                v
            })
            .unwrap_or_default();
        let children_pairs = children.map(|(v,)| v).unwrap_or_default();

        // Visibility prune at fetch time: skip walking !SHOWING subtrees.
        // This is the perf foundation of the walk on this firmware. Without
        // aurum-bootstrap running, apps register on the bus but expose huge
        // non-SHOWING tree skeletons; trying to walk them all times out.
        // aurum-bootstrap apparently does extra handshakes (cache mask on
        // the desktop, or similar) that cause apps to populate SHOWING
        // correctly; until we replicate those, this prune is what keeps the
        // walk bounded.
        if !state.showing() {
            return Node {
                sender,
                path,
                role,
                name,
                description,
                state,
                extents,
                attributes,
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
            extents,
            attributes,
            children,
        }
    }
    .boxed()
}

// ─── Transform phase ────────────────────────────────────────────────────────
//
// Pipeline is three explicit phases:
//   1. `build_tree`  — raw AT-SPI walk, no opinions. Source of truth, dumped
//      verbatim to `tvpilot-raw.txt` for inspection.
//   2. `transform_tree` — cleanup + filter + ref assignment. Consumes the
//      raw `Node` tree, produces an `AgentNode` tree shaped for an LLM:
//      pure pass-through wrappers collapsed, refs assigned in tree order,
//      class promoted from `attributes["class"]` to the displayed type.
//   3. `render_tree` — dumb stringification of the AgentNode tree.
//
// Toolkit-agnostic by construction: keep/drop is decided from AT-SPI state
// bits, role, and attribute presence — not widget naming conventions. The
// only Tizen-specific bit is the small `JUNK_TEXT` list below, isolated so
// it's easy to read, delete, or swap on platform change.

/// Strings the Samsung/Tizen Dali toolkit puts in `name`/`description` that
/// are framework noise, not user-facing content. Seeded from aurum-cli's
/// `IsStructuralContent` (`aurum_client.cpp:509`) and `IsFrameworkText`.
const JUNK_TEXT: &[&str] = &[
    "AppContent_ImageTile",
    "CommonUIPlate",
    "ImageTileBasic",
    "MainInfo",
    "InnerStroke",
    "Divider",
    "AdditionalInfo2",
    "SubText1",
    "image",
    "clipRoot",
];

fn is_junk(s: &str) -> bool {
    if JUNK_TEXT.iter().any(|j| s == *j) {
        return true;
    }
    // Short id-like strings (Tizen `Layout_1-1-3` cores) — purely digits/dashes
    // up to 7 chars, with at least one dash.
    if s.len() <= 7
        && s.contains('-')
        && s.chars().all(|c| c.is_ascii_digit() || c == '-')
    {
        return true;
    }
    false
}

/// User-visible content for this node, picking from the AT-SPI fields most
/// likely to carry it on this firmware:
///   1. `attributes["automationId"]` — Tizen's preferred way to label tiles
///      (content rows, app launcher items, settings sections)
///   2. `Description` — when distinct from `Name`
///   3. `Name` — but only when the role is in the Content tier (label,
///      heading, image, text) or Interactive tier (button, list item, …),
///      because those roles semantically use `Name` as the user-facing
///      string. For Structural/Unknown roles, `Name` is the widget class
///      and we deliberately return empty so the filter can drop the node.
fn display_label(node: &Node) -> String {
    if let Some((_, v)) = node.attributes.iter().find(|(k, _)| k == "automationId") {
        if !v.is_empty() && !is_junk(v) {
            return v.clone();
        }
    }
    if !node.description.is_empty()
        && node.description != node.name
        && !is_junk(&node.description)
    {
        return node.description.clone();
    }
    if !node.name.is_empty() && !is_junk(&node.name) {
        match role_tier(&node.role) {
            Tier::Content | Tier::Interactive => return node.name.clone(),
            _ => {}
        }
    }
    String::new()
}

/// Displayed widget type. Prefer the Dali class from `attributes["class"]`
/// (gives `"Control"`, `"Layer"`, `"TextLabel"`, `"ScrollView"` — the real
/// widget shape) and fall back to AT-SPI's `role` name. On Tizen, AT-SPI
/// `role` is `"unknown"` for nearly every Dali widget; the class attr is
/// where the truth lives.
fn display_type(node: &Node) -> String {
    if let Some((_, v)) = node.attributes.iter().find(|(k, _)| k == "class") {
        if !v.is_empty() {
            return v.clone();
        }
    }
    node.role.clone()
}

fn has_user_text(node: &Node) -> bool {
    !display_label(node).is_empty()
}

/// Agent-facing tree. Built by `transform_tree` from the raw `Node` tree
/// with refs already assigned in tree-traversal order. Rendering is just
/// stringification of this.
pub struct AgentNode {
    pub ref_id: String,
    pub typ: String,
    pub label: String,
    pub extents: Option<(i32, i32, i32, i32)>,
    pub focused: bool,
    pub state_flags: String,
    pub children: Vec<AgentNode>,
    // Bookkeeping so the action surface can map a ref back to an AT-SPI
    // object reference (sender bus-name + object path). Not consumed yet —
    // refmap is the live readout for now.
    #[allow(dead_code)]
    pub sender: String,
    #[allow(dead_code)]
    pub path: String,
}

/// Phase 2 entrypoint. Two passes:
///   1. `build_agent_tree` — bottom-up filter, produces an `AgentNode` tree
///      with empty `ref_id`s. Pure-passthrough wrappers are collapsed,
///      kept grandchildren promoted.
///   2. `assign_refs` — top-down pre-order DFS, fills in `ref_id` and
///      builds the refmap so e1 is the root, e2 its first child, …
///      (matches how a human reads the rendered output).
pub fn transform_tree(
    node: Node,
    focus: Option<&(String, String)>,
    interactive_only: bool,
) -> (Vec<AgentNode>, Vec<RefEntry>) {
    let mut roots = build_agent_tree(node, focus, interactive_only);
    let mut next_ref: u32 = 0;
    let mut refmap: Vec<RefEntry> = Vec::new();
    for root in &mut roots {
        assign_refs(root, &mut next_ref, &mut refmap);
    }
    (roots, refmap)
}

/// Bottom-up filter. A node survives if:
///   - it has user-visible text (`display_label` non-empty), OR
///   - it's interactive (focusable/sensitive/highlightable, and showing), OR
///   - it groups ≥2 kept children (a real container, not a decorative chain).
///
/// `interactive_only` tightens the rule: text-only nodes drop unless they're
/// also containers, so the agent gets just the touchable set.
fn build_agent_tree(
    mut node: Node,
    focus: Option<&(String, String)>,
    interactive_only: bool,
) -> Vec<AgentNode> {
    let children = std::mem::take(&mut node.children);
    let kept_children: Vec<AgentNode> = children
        .into_iter()
        .flat_map(|c| build_agent_tree(c, focus, interactive_only))
        .collect();

    if !node.state.showing() {
        return kept_children;
    }

    let interactive = node.state.is_interactive();
    let text = has_user_text(&node);
    let groups = kept_children.len() >= 2;
    let keep = if interactive_only {
        interactive || groups
    } else {
        interactive || text || groups
    };

    if !keep {
        return kept_children;
    }

    let label = display_label(&node);
    let typ = display_type(&node);
    let focused = node.state.focused()
        || node.state.highlighted()
        || focus
            .map(|(s, p)| s == &node.sender && p == &node.path)
            .unwrap_or(false);

    vec![AgentNode {
        ref_id: String::new(),
        typ,
        label,
        extents: node.extents,
        focused,
        state_flags: node.state.short_flags(),
        children: kept_children,
        sender: node.sender,
        path: node.path,
    }]
}

/// Pre-order DFS to assign monotonic refs (root = e1) and populate refmap.
fn assign_refs(node: &mut AgentNode, next_ref: &mut u32, refmap: &mut Vec<RefEntry>) {
    *next_ref += 1;
    node.ref_id = format!("e{}", *next_ref);
    refmap.push(RefEntry {
        sender: node.sender.clone(),
        path: node.path.clone(),
        role: node.typ.clone(),
        name: node.label.clone(),
    });
    for child in &mut node.children {
        assign_refs(child, next_ref, refmap);
    }
}

/// Phase 3: stringify the agent tree. Output shape:
///   - `[Type] "label"* @(x,y wxh) {flags} [ref=eN]`
///   - `marker` is `*` when focused, empty otherwise
///   - `label` is omitted if empty; `@(...)` if extents degenerate;
///     `{flags}` only in verbose mode.
pub fn render_tree(roots: &[AgentNode], verbose: bool) -> (String, u32) {
    let mut out = String::new();
    let mut total: u32 = 0;
    for root in roots {
        render_node(root, 0, &mut out, &mut total, verbose);
    }
    (out, total)
}

fn render_node(node: &AgentNode, indent: usize, out: &mut String, total: &mut u32, verbose: bool) {
    let pad = " ".repeat(indent * 2);
    let label_part = if node.label.is_empty() {
        String::new()
    } else {
        format!(" \"{}\"", node.label)
    };
    let marker = if node.focused { "*" } else { "" };
    let bbox_part = match node.extents {
        Some((x, y, w, h)) if w != 0 || h != 0 => format!(" @({},{} {}x{})", x, y, w, h),
        _ => String::new(),
    };
    let flags_part = if verbose {
        format!(" {{{}}}", node.state_flags)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "{}- [{}]{}{}{}{} [ref={}]\n",
        pad, node.typ, label_part, marker, bbox_part, flags_part, node.ref_id,
    ));
    *total += 1;
    for child in &node.children {
        render_node(child, indent + 1, out, total, verbose);
    }
}

/// Dump the raw `Node` tree — every fetched property, including nodes we
/// later filter out in render. Cross-check this against the rendered output
/// to see what got dropped on the floor.
pub fn dump_raw(root: &Node) -> String {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\").replace('\n', "\\n").replace('"', "\\\"")
    }
    fn tier_char(role: &str) -> char {
        match role_tier(role) {
            Tier::Interactive => 'I',
            Tier::Content => 'C',
            Tier::Structural => 'S',
            Tier::Unknown => '?',
        }
    }
    fn fmt_extents(e: &Option<(i32, i32, i32, i32)>) -> String {
        match e {
            Some((x, y, w, h)) => format!("({},{} {}x{})", x, y, w, h),
            None => "?".to_string(),
        }
    }
    fn fmt_attrs(attrs: &[(String, String)]) -> String {
        if attrs.is_empty() {
            return "{}".to_string();
        }
        let parts: Vec<String> = attrs
            .iter()
            .map(|(k, v)| format!("{}={:?}", k, v))
            .collect();
        format!("{{{}}}", parts.join(", "))
    }
    // Leaf-level "would transform keep this on its own merit?" marker — for
    // inspection only. The `>=2 kept children` rule is recursive so we don't
    // try to encode it here; comparing raw vs rendered shows the full truth.
    fn keep_marker(node: &Node) -> char {
        if node.state.showing() && (has_user_text(node) || node.state.is_interactive()) {
            '*'
        } else {
            ' '
        }
    }
    fn walk(node: &Node, indent: usize, out: &mut String) {
        let pad = " ".repeat(indent * 2);
        let bears = keep_marker(node);
        out.push_str(&format!(
            "{}[{}{}] role=\"{}\" name=\"{}\" desc=\"{}\" state=[{}] showing={} extents={} attrs={} children={} addr={}|{}\n",
            pad,
            tier_char(&node.role),
            bears,
            esc(&node.role),
            esc(&node.name),
            esc(&node.description),
            node.state.short_flags(),
            node.state.showing(),
            fmt_extents(&node.extents),
            fmt_attrs(&node.attributes),
            node.children.len(),
            node.sender,
            node.path,
        ));
        for child in &node.children {
            walk(child, indent + 1, out);
        }
    }
    let mut out = String::new();
    walk(root, 0, &mut out);
    out
}
