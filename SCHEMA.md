# tvpilot — Schema

Wire protocol, request/response shapes, snapshot grammar, error catalog.

This document is the contract between the two modes of the `tvpilot` multicall binary: **CLI mode** (default invocation) and **daemon mode** (`tvpilot daemon`). The CLI's argv parser translates user intent to `Request`. The daemon translates D-Bus reality back to `Response`. The agent never sees raw D-Bus or raw AT-SPI types.

---

## Transport

- **Socket:** `${XDG_RUNTIME_DIR}/tvpilot.sock` (typically `/run/user/<uid>/tvpilot.sock`). Mode `0600`, owned by the invoking user.
- **Framing:** length-prefixed. Each frame is `u32 LE length` followed by `length` bytes of `postcard`-encoded payload.
- **Serialization:** `postcard` 1.x with `to_allocvec` / `from_bytes`. Endianness implicit (postcard is canonical little-endian).
- **Streams:** the daemon accepts one connection at a time. A single connection may carry many request/response pairs in sequence (used by `batch` and by future long-lived sessions). The CLI today opens one connection, sends one or N requests, exits.

The CLI links no async runtime. It uses `std::os::unix::net::UnixStream`, blocking reads/writes. The daemon uses `tokio::net::UnixListener` in current-thread runtime.

---

## Envelope

Every request and every response is wrapped in the same envelope so the daemon can correlate requests in a batch and the CLI can render responses uniformly.

```rust
// Request envelope (CLI → daemon)
struct Request {
    rid: u32,              // monotonic per connection; CLI assigns
    cmd: Command,          // one of the variants below
}

// Response envelope (daemon → CLI)
struct Response {
    rid: u32,              // echo of the request rid
    result: Result_,       // Ok or Err
    snapshot: Option<Snapshot>,   // present for action / snap commands
    timing: Timing,        // server-measured durations
    daemon_meta: DaemonMeta, // version, restart flag, refmap revision
}

enum Result_ {
    Ok(Payload),
    Err(ErrorInfo),
}

struct Timing {
    total_us: u32,         // wall time inside daemon
    dbus_us: u32,          // time spent in D-Bus calls
    pathfind_us: u32,      // time spent in LRUD navigation (0 if none)
}

struct DaemonMeta {
    version: String,             // "0.1.0"
    daemon_restarted: bool,      // true on first response after daemon spawn
    snapshot_revision: u32,      // increments on every new snapshot; refs are scoped to this revision
    window_changed: bool,        // true if active window changed since last response
}
```

The CLI prints `Response` either as a human-readable rendering (default) or as JSON (`--json`). JSON is the canonical agent-facing form.

---

## Commands

Single enum, one variant per CLI subcommand:

```rust
enum Command {
    // Lifecycle
    Ping,
    Status,
    Close,
    Reset,

    // Snapshot
    Snap(SnapOpts),

    // Actions (each returns a fresh snapshot)
    Click { ref_: RefId, strategy: ClickStrategy },
    Focus { ref_: RefId },
    Text  { ref_: RefId, value: String },
    Value { ref_: RefId, value: f64 },
    Do    { ref_: RefId, action: String },
    Goto  { name: String, role_hint: Option<String> },

    // Remote
    Key   { name: KeyName, count: u8 },     // count >= 1
    Num   { digit: u8 },                    // 0-9
    Color { which: ColorButton },           // Red Green Yellow Blue

    // Batch
    Batch { items: Vec<Command>, bail: bool },
}

struct SnapOpts {
    /// Include bounds, full state list, value range/current. Default false.
    detailed: bool,
    /// Include children outside the focused window. Default false.
    all_windows: bool,
    /// Emit a per-line diff from the previous snapshot instead of the full tree.
    diff: bool,
    /// Cap rendered depth (after structural pruning). None = no cap.
    max_depth: Option<u8>,
}

enum ClickStrategy {
    Auto,        // ladder default: try direct → focus+enter → LRUD
    DirectOnly,  // refuse if direct action / GrabHighlight unavailable
    LrudOnly,    // skip direct, always navigate via key presses
}

enum KeyName {
    Up, Down, Left, Right, Enter, Back, Home, Menu,
    Play, Pause, Stop, Rewind, Forward,
    VolUp, VolDown, Mute,
    ChUp, ChDown,
    Power, Source, Info, Exit,
}

enum ColorButton { Red, Green, Yellow, Blue }

type RefId = String;   // canonical wire form: "e5". CLI accepts "e5", "ref=e5", "@e5" → "e5"
```

`Goto` is the highest-level convenience: the daemon searches the current snapshot for an interactive element matching `name` (and `role_hint` if given), then runs the click ladder against it. This is the agent's "I don't care which ref, just press the thing labelled X" verb.

---

## Payload (Ok variants)

```rust
enum Payload {
    Pong { uptime_secs: u64 },

    Status {
        version: String,
        uptime_secs: u64,
        refmap_size: u32,
        active_window: Option<WindowInfo>,
        a11y_enabled: bool,
        last_snapshot_ms_ago: Option<u32>,
    },

    Closed,                         // for Close
    ResetDone,                      // for Reset

    Snap,                           // tree carried in Response.snapshot
    Acted { path: Option<KeyPath> },// for Click/Focus/Text/Value/Do/Goto
    KeySent { repeated: u8 },       // for Key/Num/Color
    Batch { items: Vec<Response> }, // recursive
}

struct WindowInfo {
    pkg: String,        // "org.tizen.tv-viewer"
    window: String,     // "tv-viewer-graphics"
    app_bus_name: String, // ":1.39"
}

struct KeyPath {
    keys: Vec<KeyName>,
    elapsed_ms: u16,
    strategy: PathStrategy,
}

enum PathStrategy { DirectAction, FocusEnter, SpatialGreedy, GraphBfs }
```

---

## Snapshot type

```rust
struct Snapshot {
    revision: u32,                  // bumps on any change
    window: WindowInfo,
    focus: Option<RefId>,           // currently-focused element ref
    changed: bool,                  // window changed since last snapshot
    elements: Vec<Element>,         // flat list, ordered as rendered
}

struct Element {
    ref_id: Option<RefId>,          // None for non-ref-bearing rendered nodes (rare)
    indent: u8,                     // depth in the rendered tree (post-pruning)
    role: String,                   // "push_button" etc; never the raw a11y role enum
    name: String,                   // accessible name; "" if absent
    focused: bool,
    attrs: Attrs,
    value: Option<ElementValue>,    // present only if role is value-bearing and detailed=true
    bounds: Option<Bounds>,         // present only if detailed=true
}

struct Attrs {
    checked:  Option<bool>,
    expanded: Option<bool>,
    selected: bool,
    disabled: bool,
    required: bool,
    level:    Option<u8>,           // heading level
    has_children: bool,             // children pruned for context — show as collapsed
}

struct ElementValue {
    current: f64,
    min: f64,
    max: f64,
    step: f64,
    text: String,                   // value as displayed
}

struct Bounds {
    x: i32, y: i32,
    w: i32, h: i32,
    in_viewport: bool,
}
```

### Text rendering of `Snapshot`

The default human/agent form prints one line per `Element` (after pruning):

```
pkg=org.tizen.tv-viewer  win=tv-viewer-graphics  focus=e3  rev=12  changed=false
- menu_item "Home"               [ref=e1]
- menu_item "Apps"               [ref=e2]
- menu_item* "Live TV"           [ref=e3]
- menu_item "Settings"           [ref=e4]
- push_button "▶ Resume"         [ref=e5]
- toggle "Captions" off          [ref=e6]
- slider "Volume" 35/100         [ref=e7]
```

Rules for the renderer:

- One line per `Element`. Two-space indent per level.
- `<role>` first, accessible `<name>` second in double quotes (omit `""` if empty).
- `*` after the role iff `focused == true`.
- Attribute brackets `[k=v, k=v]` only for present non-default values, plus `ref=eN` last.
- For value-bearing roles, append `<current>/<max>` or value text.
- Structural roles (filler, panel, generic) are dropped entirely; their children float up.
- `--detailed` adds `[bounds=x,y,w,h]` and full attr list.

### JSON rendering

Same `Snapshot` struct serialized to JSON. `RefId` becomes `"e5"` strings in JSON for agent ergonomics.

---

## Refs in detail

```rust
type RefId = String;         // canonical wire form: "e5"
                              // CLI input accepts "e5" | "ref=e5" | "@e5"
```

Inside the daemon, a `RefEntry` carries just enough state to drive an action on the current tree:

```rust
struct RefEntry {
    bus_name:    String,       // ":1.39"
    object_path: String,       // "/org/a11y/atspi/accessible/2147496315"
    role:        String,
    name:        String,
}
```

Lifecycle:

- The `RefMap` is **cleared at the start of every snapshot computation**. Refs are assigned in tree-traversal order: first ref-bearing node is `"e1"`, second `"e2"`, and so on.
- A ref's lifetime is one snapshot. Once the daemon emits a new snapshot, all refs from the previous one are gone.
- An agent that holds a ref across snapshots and sends it back to the daemon receives `Err(RefUnknown)` plus the current snapshot, so it can re-look without an extra round trip.
- On window-stack change, `window_changed=true` is reported on the next snapshot. There is nothing extra to invalidate — refs are already implicitly invalidated by the per-snapshot clear.
- `Reset` clears the focus-graph cache and forces a fresh snapshot; the `RefMap` clear is already implied by taking a new snapshot.

No recovery path exists. Stale refs are an `Err(RefUnknown)` with a fresh snapshot attached, not a `Collection.GetMatches` search. This is intentional: duplicate sibling roles/names, transient overlays, and reordered dynamic lists all defeat handle-based recovery in ways that quietly bind the same ref to the wrong element. The simpler "refs are per-snapshot" rule cannot make that mistake.

---

## Errors

```rust
struct ErrorInfo {
    code: ErrorCode,
    msg: String,          // single-line human-readable
    hint: Option<String>, // suggested next agent step
    retryable: bool,
}

enum ErrorCode {
    // Wire / lifecycle
    BadFrame,                 // postcard decode failed
    UnknownCommand,
    DaemonNotReady,           // a11y bus not yet usable

    // Refs
    RefUnknown,               // ref is not in the current snapshot (never assigned, or stale from an earlier snapshot)

    // Actions
    ActionUnsupported,        // requested DoAction name not in element's actions
    NotFocusable,             // GrabHighlight/GrabFocus rejected, no LRUD fallback wanted
    PathNotFound,             // LRUD navigation failed within budget
    TextNotEditable,
    ValueOutOfRange,

    // Key injection
    KeyUnknown,
    KeyInjectFailed,          // efl_util input generator write failed

    // D-Bus
    A11yBusUnavailable,
    A11yEnableFailed,         // IsEnabled toggle on session bus failed
    DbusTimeout,
    DbusInvalidReply,

    // Snapshot
    DumpTreeFailed,
    DumpTreeMalformed,        // lz4 / base64 / json parse failed

    // Catch-all
    Internal,
}
```

Errors are not exceptions — every error still returns a valid `Response` envelope. `daemon_meta` and (when possible) `snapshot` accompany errors so the agent can re-orient without an extra round trip.

---

## Batch shape

```rust
// in Command
Batch { items: Vec<Command>, bail: bool }

// in Payload
Batch { items: Vec<Response> }
```

The daemon executes `items` serially. With `bail=true`, the first `Err` stops the batch and the remaining `Response` slots are absent. With `bail=false`, all items run; failures are reported per item.

Each item's `Response` carries its own `Timing` and `DaemonMeta`. The CLI prints them as an array in JSON mode or as numbered sections in human mode.

CLI surface for batch:

```bash
# from stdin
echo '[["focus","e3"],["key","down","2"],["snap"]]' | tvpilot batch
# with --bail
tvpilot batch --bail < cmds.json
```

Stdin format is the same shape as `agent-browser`'s batch — an array of argv arrays — so the CLI can reuse its own argv parser per item.

---

## Sample exchanges

### `tvpilot snap`

**Request:**
```rust
Request {
    rid: 1,
    cmd: Command::Snap(SnapOpts { detailed: false, all_windows: false, diff: false, max_depth: None }),
}
```

**Response (rendered JSON):**
```json
{
  "rid": 1,
  "result": { "Ok": { "Snap": null } },
  "snapshot": {
    "revision": 12,
    "window": { "pkg": "org.tizen.tv-viewer", "window": "tv-viewer-graphics", "app_bus_name": ":1.39" },
    "focus": "e3",
    "changed": false,
    "elements": [
      { "ref_id": "e1", "indent": 0, "role": "menu_item", "name": "Home", "focused": false, "attrs": {} },
      { "ref_id": "e2", "indent": 0, "role": "menu_item", "name": "Apps", "focused": false, "attrs": {} },
      { "ref_id": "e3", "indent": 0, "role": "menu_item", "name": "Live TV", "focused": true, "attrs": {} },
      { "ref_id": "e5", "indent": 0, "role": "push_button", "name": "▶ Resume", "focused": false, "attrs": {} },
      { "ref_id": "e6", "indent": 0, "role": "toggle", "name": "Captions", "focused": false, "attrs": { "checked": false } }
    ]
  },
  "timing": { "total_us": 18420, "dbus_us": 16100, "pathfind_us": 0 },
  "daemon_meta": { "version": "0.1.0", "daemon_restarted": false, "snapshot_revision": 12, "window_changed": false }
}
```

### `tvpilot click e7`

**Response (human form):**
```
ok
path=R,R,D,Enter (4 keys, 62ms)
---
pkg=org.tizen.tv-viewer  win=tv-viewer-graphics  focus=e7  rev=13  changed=false
- menu_item "Home"               [ref=e1]
- menu_item "Apps"               [ref=e2]
- menu_item "Live TV"            [ref=e3]
- menu_item "Settings"           [ref=e4]
- push_button "▶ Resume"         [ref=e5]
- toggle "Captions" off          [ref=e6]
- slider* "Volume" 38/100        [ref=e7]
```

### `tvpilot click e99` (stale ref from a prior snapshot)

**Response (JSON):**
```json
{
  "rid": 7,
  "result": { "Err": {
    "code": "RefUnknown",
    "msg": "Ref e99 is not present in the current snapshot",
    "hint": "re-look using the attached snapshot and reselect",
    "retryable": false
  } },
  "snapshot": { ... fresh snapshot ... },
  "timing": { "total_us": 22310, "dbus_us": 21000, "pathfind_us": 0 },
  "daemon_meta": { ..., "window_changed": true }
}
```

---

## Versioning

- `DaemonMeta.version` is a SemVer-ish string. CLI and daemon are released together; mismatched pairs respond `Err(Internal)` with a version-skew message on the first request.
- `postcard` is not self-describing. Wire compatibility is broken on any enum reordering. Pre-1.0 we make no compatibility promises; post-1.0 we add new variants only at the end of each enum and reserved trailing fields in structs.
