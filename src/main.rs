use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

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
        /// Use Collection.GetMatches to fetch focusable elements directly
        /// instead of walking the tree. Server returns matches in one call.
        #[arg(short = 'c', long)]
        collect: bool,
        /// AT-SPI bus address. Defaults to /run/user/5001/at-spi/bus
        #[arg(long)]
        bus: Option<String>,
    },
}

/// AT-SPI Collection match rule. The wire signature is `(aiia{ss}iaiiasib)`.
/// Constants per atspi-constants.h:
///   match_type 0 = INVALID, 1 = ALL, 2 = ANY, 3 = NONE, 4 = EMPTY
type StateMatchRule = (
    Vec<i32>,                                       // states (bit indices)
    i32,                                            // state match type
    std::collections::HashMap<String, String>,     // attributes
    i32,                                            // attribute match type
    Vec<i32>,                                       // roles
    i32,                                            // role match type
    Vec<String>,                                    // interfaces
    i32,                                            // interface match type
    bool,                                           // invert
);

// AT-SPI standard state bit indices (atspi-constants.h).
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
    fn has(&self, idx: u32) -> bool {
        if idx < 32 {
            (self.raw[0] >> idx) & 1 == 1
        } else if idx < 64 {
            (self.raw[1] >> (idx - 32)) & 1 == 1
        } else {
            false
        }
    }
    fn focusable(&self) -> bool { self.has(STATE_FOCUSABLE) }
    fn focused(&self) -> bool { self.has(STATE_FOCUSED) }
    fn sensitive(&self) -> bool { self.has(STATE_SENSITIVE) }
    fn showing(&self) -> bool { self.has(STATE_SHOWING) }
    fn visible(&self) -> bool { self.has(STATE_VISIBLE) }
    fn highlightable(&self) -> bool { self.has(STATE_HIGHLIGHTABLE) }
    fn highlighted(&self) -> bool { self.has(STATE_HIGHLIGHTED) }
    fn is_interactive(&self) -> bool {
        // Aurum's CLICKABLE = SENSITIVE state. We broaden slightly so
        // focusable-only elements (e.g., text entries) still surface.
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


fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Snap { interactive, verbose, collect, bus } => snap(interactive, verbose, collect, bus),
    }
}

fn snap(interactive_only: bool, verbose: bool, collect: bool, bus: Option<String>) -> Result<()> {
    let atspi_addr = bus.unwrap_or_else(default_atspi_addr);
    eprintln!("[tvpilot] at-spi bus: {}", atspi_addr);

    let atspi = zbus::blocking::connection::Builder::address(atspi_addr.as_str())?
        .build()
        .context("connect to at-spi bus")?;

    let apps = list_apps(&atspi)?;
    eprintln!("[tvpilot] {} app(s) on a11y tree", apps.len());
    for (sender, path) in &apps {
        eprintln!("[tvpilot]   {} {}", sender, path);
    }

    if apps.is_empty() {
        return Err(anyhow!(
            "no a11y apps found — is the foreground app accessibility-enabled?"
        ));
    }

    let mut printed_any = false;
    for (sender, path) in &apps {
        let r = if collect {
            collect_one_app(&atspi, sender, path, verbose)
        } else {
            dump_one_app(&atspi, sender, path, interactive_only, verbose)
        };
        match r {
            Ok(true) => printed_any = true,
            Ok(false) => {}
            Err(e) => eprintln!("[tvpilot] {} failed: {:#}", sender, e),
        }
    }
    if !printed_any {
        eprintln!("[tvpilot] no elements rendered");
    }
    Ok(())
}

fn collect_one_app(
    atspi: &Connection,
    sender: &str,
    path: &str,
    verbose: bool,
) -> Result<bool> {
    println!("# app={} path={} (Collection.GetMatches)", sender, path);
    let t0 = std::time::Instant::now();

    let root = Proxy::new(atspi, sender, path, "org.a11y.atspi.Collection")?;

    // Start with the simplest possible rule: no filter at all (everything empty
    // with MATCH_ALL so it's trivially satisfied). Once this works, we narrow.
    let rule: StateMatchRule = (
        Vec::new(),                                    // no state filter
        1,                                              // MATCH_ALL on empty = trivially true
        std::collections::HashMap::new(),
        1,                                              // MATCH_ALL on empty
        Vec::new(),                                     // any role
        1,                                              // MATCH_ALL on empty
        Vec::new(),                                     // any interface
        1,                                              // MATCH_ALL on empty
        false,
    );

    // sortby=0 (canonical), count=0 (no limit), traverse=true (whole subtree)
    let matches: Vec<(String, OwnedObjectPath)> = root
        .call("GetMatches", &(rule, 0_u32, 0_i32, true))
        .context("Collection.GetMatches")?;

    let elapsed = t0.elapsed();
    eprintln!(
        "[tvpilot] {} got {} matches in {:?}",
        sender,
        matches.len(),
        elapsed
    );

    // Render each match: role, name, state.
    for (m_sender, m_path) in &matches {
        let node = Proxy::new(atspi, m_sender.as_str(), m_path.as_str(), "org.a11y.atspi.Accessible")?;
        let role = node
            .call::<_, _, String>("GetRoleName", &())
            .unwrap_or_else(|_| "?".to_string());
        let name: String = node.get_property("Name").unwrap_or_default();
        let state = read_state(&node).unwrap_or_default();
        if !state.visible() || !state.showing() {
            continue;
        }
        let marker = if state.focused() { "*" } else { "" };
        let line_name = if name.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", name)
        };
        if verbose {
            println!("- {}{}{}  [{}]", role, marker, line_name, state.short_flags());
        } else {
            println!("- {}{}{}", role, marker, line_name);
        }
    }
    Ok(!matches.is_empty())
}

fn dump_one_app(
    atspi: &Connection,
    sender: &str,
    path: &str,
    interactive_only: bool,
    verbose: bool,
) -> Result<bool> {
    println!("# app={} path={}", sender, path);

    // Probe the Samsung DumpTree extension. If the app implements it server-side,
    // we get the whole subtree in one round trip (lz4b64-prefixed compressed JSON).
    match try_dump_tree(atspi, sender, path) {
        Ok(payload_len) => {
            eprintln!(
                "[tvpilot] {} DumpTree OK ({} payload bytes) — but walker still used for render",
                sender, payload_len
            );
        }
        Err(e) => {
            eprintln!("[tvpilot] {} DumpTree N/A: {}", sender, root_cause(&e));
        }
    }

    let printed = walk_subtree(atspi, sender, path, 0, interactive_only, verbose)?;
    Ok(printed > 0)
}

fn try_dump_tree(atspi: &Connection, sender: &str, path: &str) -> Result<usize> {
    let p = Proxy::new(atspi, sender, path, "org.a11y.atspi.Accessible")?;
    // detail_level 5 = FULL_LZ4 per aurum's AccessibleNode.h
    let payload: String = p.call("DumpTree", &5_i32).context("DumpTree call")?;
    Ok(payload.len())
}

fn root_cause(e: &anyhow::Error) -> String {
    e.chain().last().map(|c| c.to_string()).unwrap_or_default()
}

/// Standard-AT-SPI tree walker. ~4 D-Bus round trips per node
/// (GetRoleName + Name property + GetState + GetChildren). Works on any
/// AT-SPI-compliant app.
fn walk_subtree(
    atspi: &Connection,
    sender: &str,
    path: &str,
    indent: usize,
    interactive_only: bool,
    verbose: bool,
) -> Result<usize> {
    let node = Proxy::new(atspi, sender, path, "org.a11y.atspi.Accessible")?;

    let role = node
        .call::<_, _, String>("GetRoleName", &())
        .unwrap_or_else(|_| "?".to_string());
    let name: String = node.get_property("Name").unwrap_or_default();
    let state = read_state(&node).unwrap_or_default();

    let interactive = state.is_interactive();
    let show = !interactive_only || interactive;

    let mut printed = 0;
    if show {
        let pad = " ".repeat(indent * 2);
        let marker = if state.focused() { "*" } else { "" };
        let line_name = if name.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", name)
        };
        if verbose {
            println!(
                "{}- {}{}{}  [{}]",
                pad,
                role,
                marker,
                line_name,
                state.short_flags()
            );
        } else {
            println!("{}- {}{}{}", pad, role, marker, line_name);
        }
        printed += 1;
    }

    let children: Vec<(String, OwnedObjectPath)> =
        node.call("GetChildren", &()).unwrap_or_default();
    let next_indent = if show { indent + 1 } else { indent };
    for (cs, cp) in children {
        printed += walk_subtree(
            atspi,
            &cs,
            &cp.to_string(),
            next_indent,
            interactive_only,
            verbose,
        )?;
    }
    Ok(printed)
}

fn read_state(node: &Proxy<'_>) -> Result<StateBits> {
    let v: Vec<u32> = node.call("GetState", &())?;
    let raw = match v.len() {
        0 => [0, 0],
        1 => [v[0], 0],
        _ => [v[0], v[1]],
    };
    Ok(StateBits { raw })
}

fn default_atspi_addr() -> String {
    // On Tizen 10 the session bus is kdbus, which zbus does not speak. The
    // AT-SPI bus runs as a regular dbus-daemon on a unix socket at a well-known
    // path under the owner user's runtime dir, so we connect directly and skip
    // the session-bus dance entirely. The owner uid on Tizen is 5001.
    "unix:path=/run/user/5001/at-spi/bus".to_string()
}

fn list_apps(atspi: &Connection) -> Result<Vec<(String, String)>> {
    let root = Proxy::new(
        atspi,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        "org.a11y.atspi.Accessible",
    )?;
    let children: Vec<(String, OwnedObjectPath)> = root
        .call("GetChildren", &())
        .context("root GetChildren")?;
    Ok(children
        .into_iter()
        .map(|(s, p)| (s, p.to_string()))
        .collect())
}

