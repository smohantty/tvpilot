use anyhow::{Context, Result, anyhow};
use base64::Engine;
use clap::{Parser, Subcommand};
use serde_json::Value;
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
        /// Show only interactive elements (buttons, menu items, etc.)
        #[arg(short = 'i', long)]
        interactive: bool,
        /// AT-SPI bus address. Defaults to /run/user/<euid>/at-spi/bus
        #[arg(long)]
        bus: Option<String>,
    },
}

const INTERACTIVE_ROLES: &[&str] = &[
    "push button",
    "push_button",
    "menu item",
    "menu_item",
    "check box",
    "check_box",
    "radio button",
    "radio_button",
    "toggle button",
    "toggle_button",
    "slider",
    "entry",
    "text",
    "combo box",
    "combo_box",
    "list item",
    "list_item",
    "tab",
    "tree item",
    "tree_item",
    "link",
];

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Snap { interactive, bus } => snap(interactive, bus),
    }
}

fn snap(interactive_only: bool, bus: Option<String>) -> Result<()> {
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
        match dump_one_app(&atspi, sender, path, interactive_only) {
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

fn dump_one_app(
    atspi: &Connection,
    sender: &str,
    path: &str,
    interactive_only: bool,
) -> Result<bool> {
    println!("# app={} path={}", sender, path);

    // Fast path: try the Samsung DumpTree extension.
    match dump_tree_fast(atspi, sender, path) {
        Ok(tree) => {
            eprintln!("[tvpilot] {} fast path (DumpTree)", sender);
            let printed = render_dumptree_json(&tree, 0, interactive_only);
            return Ok(printed > 0);
        }
        Err(e) => {
            eprintln!("[tvpilot] {} fast path unavailable ({:#}); falling back to walk", sender, e);
        }
    }

    // Slow path: walk the tree via standard AT-SPI calls.
    let printed = walk_subtree(atspi, sender, path, 0, interactive_only)?;
    Ok(printed > 0)
}

fn dump_tree_fast(atspi: &Connection, sender: &str, path: &str) -> Result<Value> {
    let payload = dump_tree(atspi, sender, path)?;
    eprintln!("[tvpilot] {} payload {} bytes", sender, payload.len());
    let json = decode_payload(&payload)?;
    eprintln!("[tvpilot] {} decoded {} bytes JSON", sender, json.len());
    serde_json::from_str(&json).context("parse DumpTree JSON")
}

/// Standard-AT-SPI tree walker. One D-Bus round trip per node (slow but works
/// on any conformant app, including Chromium/NUI/whatever).
fn walk_subtree(
    atspi: &Connection,
    sender: &str,
    path: &str,
    indent: usize,
    interactive_only: bool,
) -> Result<usize> {
    let node = Proxy::new(atspi, sender, path, "org.a11y.atspi.Accessible")?;

    let role = node
        .call::<_, _, String>("GetRoleName", &())
        .unwrap_or_else(|_| "?".to_string());
    let name: String = node.get_property("Name").unwrap_or_default();

    let is_interactive = INTERACTIVE_ROLES.contains(&role.as_str());
    let show = !interactive_only || is_interactive;

    let mut printed = 0;
    if show {
        let pad = " ".repeat(indent * 2);
        if name.is_empty() {
            println!("{}- {}", pad, role);
        } else {
            println!("{}- {} \"{}\"", pad, role, name);
        }
        printed += 1;
    }

    let children: Vec<(String, OwnedObjectPath)> = node
        .call("GetChildren", &())
        .unwrap_or_default();
    let next_indent = if show { indent + 1 } else { indent };
    for (cs, cp) in children {
        printed += walk_subtree(atspi, &cs, &cp.to_string(), next_indent, interactive_only)?;
    }
    Ok(printed)
}

fn render_dumptree_json(node: &Value, indent: usize, interactive_only: bool) -> usize {
    render_tree(node, indent, interactive_only)
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

fn dump_tree(atspi: &Connection, sender: &str, path: &str) -> Result<String> {
    let p = Proxy::new(atspi, sender, path, "org.a11y.atspi.Accessible")?;
    // detail_level 5 = FULL_LZ4 per aurum AccessibleNode.h
    let payload: String = p.call("DumpTree", &5_i32).context("DumpTree")?;
    Ok(payload)
}

fn decode_payload(payload: &str) -> Result<String> {
    let stripped = payload.strip_prefix("lz4b64:").ok_or_else(|| {
        anyhow!(
            "payload missing lz4b64: prefix (head: {:?})",
            &payload[..payload.len().min(32)]
        )
    })?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(stripped)
        .context("base64 decode")?;
    if raw.len() < 4 {
        return Err(anyhow!("payload too small ({} bytes)", raw.len()));
    }
    let orig_len = u32::from_le_bytes(raw[0..4].try_into().unwrap()) as usize;
    let compressed = &raw[4..];
    let decompressed =
        lz4_flex::decompress(compressed, orig_len).context("lz4 decompress")?;
    String::from_utf8(decompressed).context("payload utf8")
}

fn render_tree(node: &Value, indent: usize, interactive_only: bool) -> usize {
    let role = node
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let is_interactive = INTERACTIVE_ROLES.contains(&role);
    let show = !interactive_only || is_interactive;

    let mut printed = 0;
    if show {
        let pad = " ".repeat(indent * 2);
        if name.is_empty() {
            println!("{}- {}", pad, role);
        } else {
            println!("{}- {} \"{}\"", pad, role, name);
        }
        printed += 1;
    }

    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        let next_indent = if show { indent + 1 } else { indent };
        for c in children {
            printed += render_tree(c, next_indent, interactive_only);
        }
    }
    printed
}
