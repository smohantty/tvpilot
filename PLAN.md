# tvpilot — Plan

## What it is

`tvpilot` is an accessibility-first automation surface for LLM agents driving Tizen TV applications. A CLI that an agent calls per action, backed by a long-lived local daemon that holds the D-Bus connection and per-session state. The two run on the TV target itself, communicating over a unix-domain socket with framed postcard messages.

It does for a Tizen TV what `agent-browser` does for a Chromium tab — same `eN` ref pattern, same "every action returns a snapshot" ergonomics — adapted to the focus-chain / LRUD interaction model that TVs require.

## Who it is for

The exclusive consumer is an LLM agent running on the device. Not human-typed shell scripts, not UI test suites, not other automation frameworks. Every design choice prioritizes the agent's needs over those audiences when they conflict.

Two implications:

- **Single concurrent CLI.** The daemon assumes one CLI process at a time. No multi-tenant locking, no fair scheduling. Simpler protocol, smaller binary.
- **Output is for an LLM, not a terminal.** Snapshots are compact and ref-based, not pretty-printed. Color and ANSI escapes are off by default. JSON envelope is the canonical mode.

## What it optimizes for

Two meta-objectives drive every design choice below: **per-turn performance** (an agent waiting on tvpilot is a tvpilot bug) and **on-device memory footprint** (daemon RSS, binary size, allocation churn per command). When a choice trades one for another concern, perf and memory win. In priority order:

1. **Agent context size.** A snapshot the agent reads on every turn is small and ref-shaped — strip non-actionable nodes, role-name dedup with `nth`, single-line elements, no doubled state encoding. The goal is to make context the cheap part of every agent turn. Specific size ratios versus Aurum's `DumpObjectTree` will be reported once measured on target.
2. **Per-turn latency.** Use the `atspi_accessible_dump_tree` D-Bus fast path (one round trip, LZ4 + base64 payload) instead of walking the tree node-by-node. After actions, wait for the `window:post-render` event so the returned snapshot reflects the post-action state, not the pre-action state. Concrete latency budgets are deferred until measured on target.
3. **On-device footprint.** One static-ish multicall binary (CLI and daemon modes in the same executable, dispatched in `main()` on argv[1]). No dependency on `libatspi`, `libdbus-glib`, `libglib`/`libgio`. Direct D-Bus via `zbus`. Target size: ~1.5-2 MB total.
4. **Zero on-target prerequisites beyond what the TV already has.** AT-SPI registry/bus, `libefl-util` for key injection (per Aurum's working path), `libecore-wl2` for display geometry. No new system service to install, no GMainLoop thread to babysit.

### Memory and performance discipline

A few concrete rules that follow from the meta-objectives, applied throughout the implementation:

- **Single-binary multicall** with daemon-only deps lazy-init'd out of the CLI hot path. Cold CLI start is dominated by `exec()` and dynamic linking, not crate init.
- **Per-snapshot `RefMap`**: cleared at the start of every snapshot, no long-lived ref table, no tombstone GC, no recovery search. Bounded, predictable memory.
- **Single in-flight request**: the daemon serializes commands behind one mutex. No request-queue depth to bound, no concurrent state machine.
- **Postcard wire format** on the socket — compact, allocation-light, no JSON encode/decode on the hot path (JSON renders only at the CLI's text boundary).
- **Direct `zbus`, no GLib/GIO/libatspi.** Daemon stays a single static-ish binary, no GMainLoop thread.
- **`atspi_accessible_dump_tree` fast path**: one D-Bus round trip per snapshot, LZ4 + base64 payload, never a node-by-node walk.
- **No persistent caches** except the focus-graph, which is bounded by screen signature and dropped on window change.
- **No async runtime in the CLI**: blocking unix-socket I/O only; `tokio` is daemon-side.

## Architecture

A **single multicall binary** named `tvpilot` runs in two modes:

- **CLI mode** (default, e.g. `tvpilot snap`, `tvpilot click e5`) — short-lived, one request per invocation.
- **Daemon mode** (`tvpilot daemon`) — long-lived background process. Owns the AT-SPI bus, RefMap, focus-graph cache, and IsEnabled lifecycle.

The first CLI invocation auto-spawns the daemon by re-execing `/proc/self/exe daemon` if the socket isn't live. Both modes share crate code; `main()` dispatches on argv. `daemon` is a regular public subcommand — listed in `tvpilot --help` so it's debuggable directly, even though agents typically rely on auto-spawn.

```
                 ┌──────────────────────────────────┐
                 │ LLM agent (on TV)                │
                 └──────────────────────────────────┘
                                  │ shell exec
                                  ▼
                 ┌──────────────────────────────────┐
                 │ tvpilot (single binary)          │
                 │                                  │
                 │  main() dispatches on argv[1]:   │
                 │   ─ "daemon"   → daemon::run()   │
                 │   ─ <command>  → cli::run()      │
                 │                                  │
                 │  CLI mode (foreground):          │
                 │    short-lived per turn          │
                 │    auto-spawns daemon by         │
                 │    re-execing /proc/self/exe     │
                 │    daemon if socket not live     │
                 │                                  │
                 │  Daemon mode (background):       │
                 │    holds AT-SPI bus conn,        │
                 │    RefMap, focus-graph cache,    │
                 │    IsEnabled lifecycle           │
                 └──────────────────────────────────┘
                                  │ unix socket
                                  │ framed messages
                                  ▼
                       ┌──────────┼──────────┐
                       ▼          ▼          ▼
                   AT-SPI bus  session  efl_util /
                   (unix sock) bus      input gen
                                        (for key inject)
```

### Components

One binary, two entry points:

- **CLI mode** — parses argv, connects to the daemon socket, sends one framed request, prints one response, exits. If the socket isn't live, re-execs `/proc/self/exe daemon` detached, polls the socket with backoff, then retries the request. No async runtime in this path.
- **Daemon mode** — async (tokio current-thread). Owns the AT-SPI D-Bus connection, the per-session RefMap, the focus-graph cache, and the IsEnabled lifecycle. Listens on a single unix socket. Exits N seconds after the last CLI disconnects.

`main()` is a thin dispatcher:

```rust
fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(|s| s.to_str()) {
        Some("daemon") => daemon::run(args),
        _ => cli::run(std::env::args_os().skip(1)),
    }
}
```

Daemon-only heavy deps (zbus, AT-SPI bindings) are kept out of the CLI hot path via lazy-init or feature gating where cold-start latency matters. The argv[0] symlink trick (busybox-style) is **not** used — a plain subcommand is simpler to install, simpler to debug (`tvpilot daemon` runs the daemon directly with no socket plumbing), and avoids env-var inheritance footguns when the daemon shells out to other processes.

A single session is assumed (one TV, one foreground app at a time) — no equivalent of agent-browser's `AGENT_BROWSER_SESSION` env var. The socket path is fixed; concurrent daemons are neither needed nor supported.

### Lifecycle

| Trigger | Behavior |
|---|---|
| First `tvpilot` invocation | CLI connects to socket → fails → re-execs `/proc/self/exe daemon` as detached child → polls socket up to 2 s with backoff → connects → sends request |
| Subsequent invocations | CLI connects immediately, sends request, exits — no new daemon is spawned |
| Concurrent cold-start | Two CLIs may both find no socket and both spawn a daemon. The daemon binds the unix socket as its very first action — before pid sidecar, IsEnabled toggle, or AT-SPI proxy. The loser of the `bind(2)` race gets `EADDRINUSE` and exits without side effects. Both CLIs then poll, find the surviving daemon, and connect normally |
| Daemon startup | Binds the unix socket first (atomic ownership token). On success: writes `tvpilot.pid` and `tvpilot.version` sidecars, sets `org.a11y.Status.IsEnabled = true` on the session bus, holds the proxy alive, then begins accepting connections |
| `tvpilot close` happy path | CLI sends `Close` over the socket. Daemon sets `IsEnabled = false`, releases bus connection, removes socket + sidecars, exits |
| `tvpilot close` unreachable-daemon fallback | If the daemon doesn't respond within ~500 ms but the pid in `tvpilot.pid` is alive, CLI sends `SIGKILL` to the pid then removes stale `tvpilot.sock` / `tvpilot.pid` / `tvpilot.version`. Lifted from agent-browser's `run_close_all` |
| Daemon crash | Next CLI invocation finds stale socket → checks `tvpilot.pid` → if dead, cleans sidecars and re-execs daemon. Response carries `daemon_restarted=true` so the agent knows refs are gone |
| Version skew (CLI vs running daemon) | CLI reads `tvpilot.version` before connecting. On mismatch, sends `Close`, waits, then re-execs the matching daemon |
| Concurrent CLI access | Daemon accepts multiple connections; commands are serialized behind an internal mutex. A second CLI that connects while the first is mid-command simply waits — no `BUSY` error. The response's `snapshot_revision` lets the second CLI's agent detect "the world moved while I was waiting" |

The CLI and daemon share crate code but mode-specific paths are gated so the CLI's cold-start cost stays dominated by `exec()` and dynamic linking, not by daemon-side crate init.

## Talking to AT-SPI

We talk to AT-SPI as a pure D-Bus client via `zbus`. No `libatspi` on disk, no `libdbus-glib`, no GLib/GIO. The end-to-end path was proven in a prior PoC against the live target:

- AT-SPI bus is a regular unix socket at `/run/user/<uid>/at-spi/bus`.
- Standard `org.a11y.atspi.*` interfaces are introspectable and callable over plain D-Bus.
- Samsung's Tizen extensions (`DumpTree`, `GetNodeInfo`, `GetStringProperty`, `GetNeighbor`, `SetIncludeHidden`, `SetListenPostRender`, `DoGesture`) are exposed as plain D-Bus methods on the standard `org.a11y.atspi.Accessible` interface — Aurum's code at the C-API level confirms there is no hidden Samsung-only D-Bus interface.
- `IsEnabled` on the **session bus** is toggled by plain `Properties.Set` on `org.a11y.Bus`. Aurum's reference uses GDBus over the session bus for the exact same property write, so there is no `kdbus` requirement and no need for a `libdbus-1` dependency for this one call.

The implementation revisits this only if a specific zbus call surprises us on target during 0.1 work. We are not currently planning a `libatspi` fallback path.

## Key injection

Raw keys go through Tizen's input generator service via `libefl-util`, mirroring Aurum's working path:

- `efl_util_input_initialize_generator(EFL_UTIL_INPUT_DEVTYPE_KEYBOARD)` once at daemon startup.
- `efl_util_input_generate_key(handle, "Down", 1)` for press, `0` for release. Repeat sleeps are caller-controlled (`tvpilot key down 3` issues three press/release pairs).
- Touch and pointer generators (`EFL_UTIL_INPUT_DEVTYPE_TOUCHSCREEN`, `EFL_UTIL_INPUT_DEVTYPE_POINTER`) are out of scope for v0.1 but the same library exposes them when we want them.

`uinput` and Wayland virtual keyboard are **not** used — they're either privileged differently or absent on retail Tizen TV. The earlier "uinput / Wayland" framing in this doc was wrong.

## Surface (commands)

Three families. All commands return a snapshot in the same response unless explicitly stateless.

### Snapshot family

- `tvpilot snap` — current screen, compact text form
- `tvpilot snap --json` — same data, structured
- `tvpilot snap --all` — include bounds, full states, values (debug/diagnostics)
- `tvpilot snap --diff` — diff from last snapshot in this session

### Action family (each returns a fresh snapshot)

- `tvpilot click <ref>` — try direct AT-SPI action; fall back to focus + Enter; fall back to LRUD path-finding
- `tvpilot focus <ref>` — same path-finding, no Enter
- `tvpilot text <ref> "<value>"` — set text via EditableText
- `tvpilot value <ref> <number>` — set value (sliders)
- `tvpilot do <ref> <action-name>` — generic AT-SPI action by name
- `tvpilot goto "<name>"` — find by visible name then click (highest level)

### Remote family (raw key injection)

- `tvpilot key <name>` — single key (`right`, `down`, `up`, `left`, `enter`, `back`, `home`, `menu`, `play`, `pause`, `stop`, `volup`, `voldown`, etc.)
- `tvpilot key <name> <count>` — repeat
- `tvpilot num <0-9>` — numeric keys
- `tvpilot color <red|green|yellow|blue>` — color buttons

### Session / lifecycle

- `tvpilot ping` — daemon health
- `tvpilot status` — daemon uptime, refmap size, last snapshot age
- `tvpilot close` — gracefully stop daemon, clear IsEnabled
- `tvpilot reset` — clear RefMap, focus-graph cache, force fresh snapshot
- `tvpilot batch < cmds.json` — run multiple commands in one connection (see below)

## Snapshot output shape

```
pkg=org.tizen.tv-viewer  win=tv-viewer  focus=e3  rev=12
- menu_item "Home"               [ref=e1]
- menu_item "Apps"               [ref=e2]
- menu_item* "Live TV"           [ref=e3]
- menu_item "Settings"           [ref=e4]
- push_button "▶ Resume"         [ref=e5]
- toggle "Captions" off          [ref=e6]
```

Header line carries the package, window, currently-focused ref, and a snapshot revision number. `*` after the role marks the focused element. Bounds, full state lists, values are off the screen unless `--all` is passed.

After an action that required LRUD navigation, the header gets one extra field:

```
pkg=org.tizen.tv-viewer  win=tv-viewer  focus=e5  rev=13  path=R,R,D,Enter (4 keys, 62ms)
```

So the agent learns navigation cost over time and decides whether to nav now or defer.

## Ref design (lifted from agent-browser)

- Format: `eN`, e.g. `e5`, `e23`. CLI accepts `e5`, `ref=e5`, `@e5`. Wire form is the same string.
- Refs are **per-snapshot, monotonic, and cleared on every new snapshot.** This matches agent-browser's actual implementation. The daemon assigns refs in tree-traversal order: first ref-bearing node is `e1`, second `e2`, and so on. The `RefMap` is emptied at the start of every snapshot computation.
- Each `RefEntry` stores just enough to drive an action on the current tree: `(bus_name, object_path, role, name)`. No `parent_name`, `nth`, `window_id`, blake3 handle, or tombstone state — none are needed when a ref's lifetime is bounded by one snapshot.
- After an action (`click`, `focus`, `text`, `value`, `do`, `goto`), the daemon takes a fresh snapshot before responding. The response carries that new snapshot, which has its own ref space starting again at `e1`. Refs are scoped to the snapshot they came from.
- If the agent reuses an old ref after a new snapshot has invalidated it, the daemon responds `Err(RefUnknown)` with the current snapshot attached, so the agent can re-look without an extra round trip.
- On window-stack change (a new top-level window activates), the next snapshot's header carries `window_changed=true`. Refs are already implicitly invalidated by the per-snapshot clear; the flag just lets the agent know its mental model of "the screen" has shifted.

This trades session stability for simplicity. The cost: an agent that wants to remember "the volume slider" across snapshots must re-identify it by role + name each turn. The benefits:

- No collision risk on duplicate sibling roles/names (the original blake3 scheme aliased two identical buttons under the same parent).
- No tombstone GC and no `Collection.GetMatches` recovery path against an AT-SPI match-rule that may not even be expressible.
- No "the same ref now points at a different element" footgun across snapshot boundaries.

## Role classification

Three tiers, lifted directly from agent-browser:

- **Interactive roles** always get a ref: `push_button`, `menu_item`, `menu_item_radio`, `menu_item_check`, `check_box`, `radio_button`, `toggle_button`, `slider`, `entry`, `combo_box`, `list_item`, `tab`, `tree_item`
- **Content roles** get a ref only if their name is non-empty: `heading`, `label`, `image`, `tooltip`, `table_cell`, `column_header`, `row_header`
- **Structural roles** never get a ref and are dropped from the rendered output (children render at the same indent level): `filler`, `redundant_object`, `panel`, `container`, `scroll_pane`, `viewport`, `section`, `page_tab_list`, `window`, `application`

The TV equivalent of agent-browser's "cursor-interactive promotion" is **focus-chain promotion**: any node reached by `GetNeighbor(prev → next)` walk that doesn't already match an interactive role still gets a ref. This catches custom-rendered focusable widgets that don't declare a standard role.

## Click ladder (the navigation problem)

Tizen TVs distinguish two related concepts: **highlight** (the visible TV-navigation focus that moves with LRUD keys, usually shown by a glow/scale animation) and **input focus** (the AT-SPI state that determines where keyboard events route). Aurum keeps them separate, and so do we — they map to `Component.GrabHighlight()` and `Component.GrabFocus()` respectively.

Every `click <ref>` runs this ladder, stopping at the first rung that succeeds:

1. Resolve `<ref>` → live `AtspiAccessible` from the current snapshot's `RefMap`. If the ref isn't there, return `Err(RefUnknown)` with a fresh snapshot.
2. Already highlighted? → send `Enter` key → done.
3. Has an accessible action named `click` / `activate`? → `DoActionName(...)` → done.
4. Highlightable directly? → `GrabHighlight()` (TV-nav focus), then `Enter` → done. If the element does not support highlight, fall back to `GrabFocus()` + `Enter`.
5. LRUD pathfinding:
   - **Online spatial greedy** first: compute target direction from current highlight, press best key, observe the new highlight, accept on progress, retry with second-best on no-op. Cap at 12 steps.
   - **Focus-graph BFS** if step 5a stalls or has already failed on this screen: dry-walk the highlight chain via successive key presses, build a directed graph keyed by `(node, direction)`, BFS to target, replay keys. Cache the graph by screen signature.
6. After the action fires, toggle `SetListenPostRender(true)`, wait for the `window:post-render` event (with a timeout), toggle it back off, then take a fresh snapshot. Attach the `path=...` header in the response.

Most clicks land at rungs 2-4. Rung 5 is the bounded fallback. The post-render wait at rung 6 is what makes the returned snapshot reflect the resulting UI state rather than the pre-action state.

## Batching (`tvpilot batch`)

Lifted from `agent-browser` with adaptations:

```jsonc
[
  ["focus", "e3"],
  ["key", "down", "2"],
  ["snap"]
]
```

Sent on stdin; daemon executes serially over one socket connection. Response is an array of envelopes in order. `--bail` stops on the first error.

Where this matters for tvpilot:

- **CLI process startup is amortized** across N commands. Per-call cost ≈ 5-15 ms; batching 5 commands saves ~40 ms wall time.
- **Atomic-feeling sequences:** "focus then send key 3 times then snapshot" → one batch, one consistent final snapshot.
- **No mid-batch agent decisions.** If the agent needs to branch on intermediate state, it should issue separate calls. Batches are for fixed sequences.

The per-action auto-snapshot makes batching less critical than in `agent-browser`. Most agent turns are a single action; batches are an optimization for the cases where the agent already knows the next 2-3 steps.

## Out of scope (this phase)

- Screenshots (image capture requires Wayland/TBM bindings; punt to v0.2 or shell out to Aurum)
- App install / uninstall (Tizen `pkgmgr` integration is a separate concern)
- App launch via deeplinks (this is what the original tvpilot scope was; we may layer it back on top later)
- TV control (volume, channel, input source) — except via `tvpilot key`, which is sufficient for now
- Recording / replay
- Cross-target federation
- Public language bindings (single-process exec is the only interface)
- **Overlay-aware ref preservation across window-stack changes** (e.g., a transient volume OSD popping over the main app). For v0.1 we trust AT-SPI window events; Aurum's `org.enlightenment.wm GetVisibleWinInfo_v3` side channel is a v0.2 enhancement

## Roadmap

| Phase | Deliverable |
|---|---|
| 0.0 | Cargo-tizen project scaffold, armv7l release build, install to target via `rsdb`, smoke test. |
| 0.1 | Daemon connects to AT-SPI bus via `zbus`, owns IsEnabled via plain session-bus property write, accepts unix socket, responds to `ping`. CLI auto-spawns daemon. |
| 0.2 | `snap` returns compact text snapshot via `DumpTree` fast path + LZ4 + base64 decode. Per-snapshot `RefMap` (monotonic, cleared each snapshot). |
| 0.3 | `click` ladder rungs 1-4 (direct AT-SPI action, `GrabHighlight`, `GrabFocus`). `key` raw injection via `libefl-util` (`efl_util_input_generate_key`). |
| 0.4 | LRUD pathfinding (spatial greedy). Focus-graph BFS as fallback. `SetListenPostRender` + `window:post-render` wait around actions. |
| 0.5 | `batch` mode, snapshot `--diff` (text diff against previous), `goto` verb. |
| 0.6 | Field hardening with real agent traffic. Performance and memory measurement against the meta-objectives. |
| 1.0 | Stable wire schema, documented public commands. Overlay-aware refs via WM side channel. |
