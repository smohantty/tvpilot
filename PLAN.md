# tvpilot — LLM-Native CLI for Tizen TV (On-Device)

## Name: `tvpilot`

An agent "pilots" the TV — short, memorable, tab-completable. Domain-specific without being platform-locked.

---

## Core Philosophy

The agent and `tvpilot` both live on the TV. `tvpilot` exposes the **smallest composable primitives**. Each subcommand does exactly ONE atomic thing. The agent composes them via pipes, `&&` chains, subshells, and scripts.

tvpilot does **not** orchestrate. It does **not** bundle multi-step flows. It is a box of Lego bricks, not a pre-built model.

---

## Design Principles

1. **Smallest composable unit** — Each subcommand does exactly ONE atomic thing. The agent composes.
2. **Agent-first** — No interactive prompts, no color, no spinners. Structured JSON by default.
3. **Self-describing** — `schema`, `capabilities`, and `health` let the agent discover what is available and ready at runtime.
4. **Pipe-native** — One-shot commands emit exactly one JSON envelope on stdout. Streaming commands emit one envelope per line. Commands can read prior JSON from stdin via `--stdin`.
5. **Idempotent where possible** — Repeated calls produce the same result or a no-op.
6. **Fail loud, fail structured** — Errors are typed JSON with machine-readable codes, retry hints, and suggested next actions.
7. **Guarded references** — App IDs and content IDs may be durable; UI node IDs are snapshot-scoped and must be used with the tree revision and expected app context that produced them.
8. **Capability-gated** — Discovery must expose backend/provider-specific support so the agent only plans with affordances that actually exist on this TV.
9. **Minimal lookup per command** — A targeted command resolves only the target it needs. It must not trigger hidden global discovery such as a full app inventory unless the command explicitly asks for it.
10. **Forward-compatible** — New capabilities appear as new commands or scopes. The envelope never changes shape.

---

## I/O Contract

### Output Envelope (stdout)

Single-shot commands emit exactly one JSON envelope on one line.

Streaming commands emit one JSON envelope per line (NDJSON) using the same top-level keys.

```jsonc
{"ok":true,"cmd":"app.launch","rid":"req-42","data":{...},"ts":"2026-03-14T10:23:01Z","ms":12,"v":"0.1.0"}
```

Expanded:

```jsonc
{
  "ok": true,                    // bool — success?
  "cmd": "app.launch",          // string — which subcommand ran
  "rid": "req-42",              // string|null — echo of --request-id if supplied
  "data": { ... },              // object — payload (present when ok=true)
  "error": {                    // object — present when ok=false
    "code": "APP_NOT_FOUND",
    "msg": "org.foo not installed",
    "retryable": false,
    "hint": "app list"
  },
  "ts": "2026-03-14T10:23:01Z", // ISO 8601
  "ms": 12,                     // wall-clock duration
  "v": "0.1.0"                  // schema version
}
```

Notes:

- `data` always contains the command-specific payload. For streaming commands like `observe`, each line is its own envelope and `data` carries the event payload.
- Commands that need shell-friendly scalar output may offer `--raw`, but without `--raw` they still emit the standard envelope.

### Exit Codes

| Code | Meaning |
|------|---------|
| 0    | Command executed successfully (`ok: true`), even if the payload says "not installed", "empty", or similar business state |
| 1    | Command failed operationally (`ok: false`, see `error.code`) |
| 2    | Usage error (bad args, unknown subcommand) |

### Input Conventions

- Positional args for the primary target: `tvpilot app launch netflix`
- Flags for modifiers: `--limit 5`, `--provider netflix`
- `--stdin`: read JSON envelope or raw JSON from stdin (for piping previous command output)
- `--request-id <id>`: caller-generated correlation ID, echoed back in `rid`
- `--timeout-ms <ms>`: per-command timeout override
- `--field <path>`: repeated flag to request partial response (reduces token cost)
- `--raw`: commands that support scalar extraction can emit raw output instead of the standard envelope
- For app/provider targets, prefer canonical handles like `netflix`, `youtube`, `tv_plus`; exact `app_id` is still accepted as an escape hatch
- Long flags only — agents don't need brevity, they need clarity

### Composability Patterns

```bash
# Pipe: search and launch directly from a result's launch object
tvpilot content search --query "dark knight" --limit 1 | tvpilot app launch --stdin --path .data.results[0].launch

# Chain: sequential with exit-code gating
tvpilot app launch netflix --uri "netflix://watch/70143836" && tvpilot wait idle && tvpilot volume set 20

# Subshell capture
tvpilot app launch netflix --uri "$(tvpilot content resolve --title "dark knight" | tvpilot pick .data.launch.uri --stdin --raw)"

# Parallel
tvpilot app foreground & tvpilot volume get & tvpilot play status & wait

# Script
result=$(tvpilot content search --query "stranger things" --provider netflix --limit 1)
uri=$(echo "$result" | tvpilot pick .data.results[0].launch.uri --stdin --raw)
app=$(echo "$result" | tvpilot pick .data.results[0].launch.app --stdin --raw)
tvpilot app launch "$app" --uri "$uri"
```

---

## Identity Model

Every app/provider exposed to the agent has up to three identifiers:

- `handle` — the canonical, CLI-facing stable slug chosen by `tvpilot`, such as `netflix`, `youtube`, or `tv_plus`. This is what the agent should plan with and pass to commands by default.
- `display_name` — the human-facing name shown in UI and logs, such as `Netflix`, `YouTube`, or `Samsung TV Plus`. This may be localized or vary slightly by firmware.
- `app_id` — the exact platform-native identifier used by Tizen launch/install/runtime APIs, such as `org.tizen.tvplus` or a provider-specific package ID. This is precise but not a good primary planning token for an LLM.

Rules:

- App-targeting commands should accept `<app_ref>`, where `<app_ref>` may be either a `handle` or an exact `app_id`.
- The agent should prefer `handle`; `app_id` remains available for debugging, exact low-level control, and integration with native launchers.
- Responses should return `handle`, `display_name`, and `app_id` whenever known so the agent can reason semantically while still keeping an exact execution target.
- In content commands, the `provider` field uses the same canonical handle space as app `handle`.

---

## Execution Model

`tvpilot` should be a thin command surface over a small set of internal authorities. It should not behave like a hidden inventory scanner on every invocation.

### Responsibility Split

- **CLI process (`tvpilot`)** — parses args, enforces the output/error contract, performs minimal per-command orchestration, resolves `handle`/`app_id` references, and issues targeted reads/writes to the appropriate backend.
- **Platform authority** — the source of truth for live runtime facts such as installed apps, running apps, foreground app, launchability, playback hooks, source switching, volume, and key injection. This may be a native Tizen API or a local system service wrapping those APIs.
- **Optional local daemon/service** — owns long-lived subscriptions, `wait *`, `observe`, expensive discovery reuse, and warm caches that benefit repeated CLI invocations. The CLI may query it when present but must not require it for every basic command.
- **Persistent store / DB** — holds durable metadata such as canonical-handle registries, local overrides, learned mappings, and last-known snapshots. It is not the primary authority for volatile runtime state.

### Source-of-Truth Rules

- If a fact can change outside `tvpilot` and must be correct now, it should come from the platform authority or a daemon that mirrors platform events.
- Persistent storage may accelerate resolution, but live runtime state always wins when cache and runtime disagree.
- Undocumented platform DBs may be used only as a fallback or enrichment path, not as the primary contract for live app/runtime state.

### Lookup Policy

- **Explicit global discovery commands** may enumerate broadly: `app list`, `app list --running`, `capabilities`, `health`, and provider discovery commands.
- **Targeted commands** must do only targeted work: `app launch`, `app close`, `app installed`, `app info`, `wait app`, and similar commands must not implicitly call `app list` first.
- A targeted app command should resolve `<app_ref>` using the cheapest path that can be correct: direct `app_id`, explicit local override, bundled handle registry, daemon-resolved mapping, then targeted validation against the platform authority.
- If targeted validation finds no match, return `APP_NOT_FOUND`. If it finds more than one plausible match, return `AMBIGUOUS_APP_REF` with candidates rather than guessing.

### Cache Policy

- Safe to cache: `handle`, `display_name`, candidate `app_id`s, app type, version, adapter metadata, provider metadata, and learned overrides.
- Do not treat as cache-authoritative: installed-now, running-now, foreground-now, pid, readiness, playback-now, current UI tree, and other volatile state.
- The daemon may keep warm caches for performance, but command semantics still reflect live reality, not stale snapshots.

### Handle Resolution

For a command like `tvpilot app launch netflix`, the expected path is:

1. Resolve `netflix` through an override map or bundled handle registry to one or more candidate `app_id`s.
2. Validate the candidate(s) against the platform authority with targeted checks instead of enumerating the full app list.
3. If exactly one candidate is valid, use it and return the resolved `{handle, display_name, app_id}` in the response.
4. If no candidate is valid, return `APP_NOT_FOUND`.
5. If multiple candidates are valid, return `AMBIGUOUS_APP_REF` with candidate identities.

This keeps frequent LLM-composed invocations cheap while preserving exact execution.

---

## Command Reference

Naming: `<noun> <verb>` — the noun is the domain, the verb is the single action.

---

### 1. Introspection

| Command | Does exactly | Output `data` |
|---|---|---|
| `schema` | Full command catalog with args, types, defaults | `{version, commands: [{name, description, args, example}]}` |
| `schema --command <name>` | One command's contract | single command object |
| `capabilities` | What this TV can do (runtime, hardware, automation, media, content) | `{runtime: {...}, hardware: {...}, automation: {...}, media: {...}, content: {...}}` |
| `capabilities --scope <name>` | One capability scope | single scope object |
| `health` | CLI + platform readiness check | `{ready: bool, checks: [{name, ok, msg}]}` |

`capabilities` tells the agent what's available *before* it tries. Example: does this TV have a tuner? Can it do screen capture? Is accessibility-tree inspection supported?

`capabilities` must expose backend detail, not just booleans. Example: whether observation is `screen_only` vs `ui_tree`, whether playback is `keys_only` vs `provider_observable`, which content providers have on-device adapters, which canonical handles resolve to which installed `app_id`, and whether an optional helper daemon/resolver service is available.

`health` is a pre-flight check: are Tizen APIs accessible, is the automation backend up, can we inject keys, and if a helper daemon exists is it reachable and in sync.

---

### 2. System

| Command | Does exactly | Output `data` |
|---|---|---|
| `system info` | Hardware/software identity | `{model, os, firmware, resolution, hostname}` |
| `system memory` | Memory usage | `{total_mb, free_mb, used_mb}` |
| `system storage` | Disk usage | `{total_mb, free_mb, used_mb}` |
| `system uptime` | Uptime | `{uptime_sec}` |
| `system network` | Network state | `{type, ip, connected, ssid}` |

---

### 3. App

| Command | Does exactly | Output `data` |
|---|---|---|
| `app list` | All installed apps | `{apps: [{handle, display_name, app_id, version, type}]}` |
| `app list --running` | Only running apps | `{apps: [{handle, display_name, app_id, pid, state}]}` |
| `app info <app_ref>` | One app's metadata and resolved identity | `{handle, display_name, app_id, version, type, state, installed_size_kb}` |
| `app launch <app_ref>` | Launch/foreground an app | `{handle, display_name, app_id, pid}` |
| `app launch <app_ref> --uri <deeplink>` | Launch with deep-link | `{handle, display_name, app_id, pid, uri}` |
| `app launch <app_ref> --extra key=val` | Launch with app_control extras (repeatable) | `{handle, display_name, app_id, pid, extras: {k:v}}` |
| `app launch --stdin` | Launch from a raw launch object read from stdin | `{handle, display_name, app_id, pid, uri, backend}` |
| `app launch --stdin --path <path>` | Launch from a launch object nested inside stdin JSON | same |
| `app close <app_ref>` | Terminate a running app | `{handle, display_name, app_id, was_running}` |
| `app foreground` | Which app is currently focused | `{handle, display_name, app_id, pid}` |
| `app installed <app_ref>` | Check if an app is installed (always exits 0 unless the probe itself fails) | `{handle, display_name, app_id, installed: bool}` |
| `app install <path>` | Install .wgt/.tpk from local path | `{handle, display_name, app_id, version}` |
| `app uninstall <app_ref>` | Remove an app | `{handle, display_name, app_id}` |

Where an app command takes `<app_ref>`, it accepts either the canonical `handle` or the exact `app_id`. The CLI should resolve the reference internally and return the fully resolved identity in the response.

`app launch --stdin` consumes a normalized launch object shaped like `{app, display_name, app_id, uri, extras, backend}`. The `app` field is the canonical handle accepted by `app launch <app_ref>`. When stdin contains a larger envelope, `--path` extracts the nested launch object to consume.

`app list` is an explicit discovery command. Commands like `app launch`, `app close`, `app installed`, and `wait app` must not implicitly enumerate the full app list first.

---

### 4. Inspect

Observation-only commands for reading screen and UI state. Separated from mutation commands.

| Command | Does exactly | Output `data` |
|---|---|---|
| `inspect app` | Current foreground app | `{handle, display_name, app_id, pid}` |
| `inspect screen` | Capture current screen | `{path, width, height, size_bytes}` |
| `inspect screen --base64` | Capture as inline base64 | `{width, height, encoding, image}` |
| `inspect screen --region <x,y,w,h>` | Capture a region | same |
| `inspect playback` | Current playback state | `{backend, observed, state, content_id, title, position_ms, duration_ms, speed, audio_track, subtitle}` |
| `inspect ui` | Machine-readable accessibility/UI tree | `{tree_rev, app_handle, app_display_name, app_id, captured_at, root: {node_id, role, label, ...children}}` |
| `inspect ui --depth <n>` | Limit tree depth (default 4) | same |
| `inspect ui --root <node_id>` | Subtree from a specific node | same |
| `inspect ui --focused-only` | Only the focused path | same |
| `inspect ui --visible-only` | Only visible nodes | same |

**UI tree node shape:**

```jsonc
{
  "node_id": "n-142",
  "role": "button",
  "label": "Play",
  "text": "",
  "bounds": { "x": 120, "y": 400, "w": 200, "h": 60 },
  "focused": true,
  "enabled": true,
  "visible": true,
  "actions": ["activate", "focus"],
  "children": []
}
```

Node IDs are only valid within the returned `tree_rev`.

Any command that acts on a node should supply `--tree-rev <rev>` and may additionally guard with `--expect-app <app_ref>`. If the UI rerenders or the app changes, the command fails with `STALE_REFERENCE` or `PRECONDITION_FAILED` instead of acting on the wrong target.

This is what enables the agent to reason about screen structure instead of blindly pressing keys.

---

### 5. UI

Mutation commands that interact with UI elements by node ID (obtained from `inspect ui`). They should be guarded by snapshot metadata to avoid stale actuation.

| Command | Does exactly | Output `data` |
|---|---|---|
| `ui focus <node_id> --tree-rev <rev>` | Move focus to a node from a specific snapshot | `{node_id, tree_rev, focused: true}` |
| `ui activate <node_id> --tree-rev <rev>` | Activate a node (click/select) from a specific snapshot | `{node_id, tree_rev, action: "activate"}` |
| `ui activate <node_id> --tree-rev <rev> --action <name>` | Specific action: `open`, `play`, `select` | same |
| `ui input <node_id> --tree-rev <rev> --value <text>` | Type text into a specific field node | `{node_id, tree_rev, value}` |
| `ui input <node_id> --tree-rev <rev> --value <text> --submit` | Type and submit | same |
| `ui text --query <text>` | Find visible nodes by text match in the current snapshot | `{tree_rev, app_handle, app_display_name, app_id, matches: [{node_id, role, label, text, bounds}]}` |
| `ui text --query <text> --exact` | Exact match only | same |
| `ui path` | Focus/back navigation history | `{app_handle, app_display_name, app_id, path: [{node_id, label, role}]}` |

All node-targeting `ui` commands should also accept `--expect-app <app_ref>` to fail fast if the foreground app changed since inspection.

---

### 6. Navigation (low-level)

Raw key/text injection. Use when UI tree is unavailable or insufficient.

| Command | Does exactly | Output `data` |
|---|---|---|
| `nav press <key> [key...]` | Inject key presses sequentially | `{keys, count}` |
| `nav press <key> --repeat <n>` | Repeat one key N times | `{keys, repeat, count}` |
| `nav press <key> --hold-ms <ms>` | Long-press | `{key, held_ms}` |
| `nav press <key> --delay <ms>` | Delay between keys (default 50ms) | same |
| `nav text --value <text>` | Type text into focused field | `{text, length}` |
| `nav text --value <text> --submit` | Type and press enter | same |
| `nav home` | Go to home screen | `{}` |
| `nav back` | Send back | `{}` |
| `nav back --repeat <n>` | Multiple backs | `{count}` |
| `nav keys` | List all supported key names | `{keys: [...]}` |

**Supported keys:** `UP`, `DOWN`, `LEFT`, `RIGHT`, `ENTER`, `BACK`, `HOME`, `MENU`, `PLAY`, `PAUSE`, `PLAY_PAUSE`, `STOP`, `REWIND`, `FAST_FORWARD`, `VOLUME_UP`, `VOLUME_DOWN`, `MUTE`, `CHANNEL_UP`, `CHANNEL_DOWN`, `0`-`9`, `RED`, `GREEN`, `YELLOW`, `BLUE`, `INFO`, `SOURCE`, `POWER`

---

### 7. Content

These commands are adapter-backed. The agent should only plan against providers and operations advertised by `capabilities.content` and `content providers`.

If a provider lacks a local adapter, it should be absent or marked non-searchable/non-resolvable rather than inviting speculative calls that end in `UNSUPPORTED`.

| Command | Does exactly | Output `data` |
|---|---|---|
| `content search --query <text>` | Free-text search across providers with local adapters | `{query, results: [{content_id, title, type, provider, provider_display_name, year, rating, synopsis, launchable, auth_required, launch: {app, display_name, app_id, uri, extras, backend}, adapter, confidence}]}` |
| `content search --query <text> --provider <handle>` | Search one provider adapter | same |
| `content search --query <text> --type <movie\|series\|live>` | Filter by type | same |
| `content search --query <text> --limit <n>` | Cap results (default 10) | same |
| `content search --query <text> --cursor <cursor>` | Pagination | same + `{cursor}` |
| `content discover --category <name>` | Browse: `trending`, `recommended`, `continue_watching`, `new` | `{category, items: [{content_id, title, ...}]}` |
| `content discover --category <name> --provider <handle>` | Category within one provider | same |
| `content resolve --title <title>` | Turn a title into a launch-ready result | `{content_id, title, provider, provider_display_name, launchable, auth_required, launch: {app, display_name, app_id, uri, extras, backend}, adapter, confidence}` |
| `content resolve --title <title> --provider <handle>` | Resolve a title within one provider | same |
| `content resolve --content-id <id>` | Resolve by ID | same |
| `content info <content_id>` | Full metadata | `{content_id, title, type, provider, provider_display_name, year, rating, synopsis, seasons, episodes, cast, launch}` |
| `content providers` | List available providers and adapter capabilities | `{providers: [{handle, display_name, app_id, installed, searchable, resolvable, launchable, authenticated, adapter}]}` |

`content search` should return launch-ready results whenever the provider adapter can produce them. Each result includes a normalized `launch` object that `app launch` can consume directly.

Normalized launch object shape:

```jsonc
{
  "app": "netflix",            // canonical app handle accepted by `app launch`
  "display_name": "Netflix",
  "app_id": "org.netflix",
  "uri": "netflix://watch/80057281",
  "extras": {},
  "backend": "deeplink"
}
```

`content resolve` is the critical bridge: "I know the title" → "here is the best launch object for it." It should return `launchable`, the normalized `launch` object, `adapter`, and `confidence` so the agent knows what to execute and whether to trust or verify.

---

### 8. Playback

Dedicated playback control with structured state when a provider/backend exposes it. More reliable than key injection for media control.

If only media-key injection exists, `capabilities.media` must say so explicitly and the agent should use `nav press` plus `inspect`/vision fallback instead of assuming rich playback telemetry exists.

| Command | Does exactly | Output `data` |
|---|---|---|
| `play start --content-id <id>` | Start playing content via a supported backend | `{content_id, backend, state}` |
| `play start --launch-uri <uri>` | Start via deep-link URI | `{launch_uri, backend, state}` |
| `play start ... --resume` | Resume from last position | same |
| `play pause` | Pause | `{backend, state: "paused", position_ms}` |
| `play resume` | Resume | `{backend, state: "playing", position_ms}` |
| `play stop` | Stop | `{backend, state: "stopped"}` |
| `play seek --position-ms <ms>` | Seek to absolute position | `{backend, position_ms}` |
| `play seek --delta-ms <ms>` | Seek relative (positive=forward, negative=back) | `{backend, position_ms, delta_ms}` |
| `play status` | Current playback state | `{backend, observed, state, content_id, title, position_ms, duration_ms, progress_pct, speed, audio_track, subtitle}` |

---

### 9. Wait

Blocking primitives so the agent doesn't write poll loops. Each one blocks until a condition is met or `--timeout-ms` is exceeded.

| Command | Does exactly | Output `data` |
|---|---|---|
| `wait app --app <app_ref> --state <state>` | Block until app reaches `foreground`/`background`/`closed`/`ready` | `{handle, display_name, app_id, state, waited_ms}` |
| `wait idle` | Block until screen is stable (no transitions) | `{waited_ms}` |
| `wait playback --state <state>` | Block until playback reaches `playing`/`paused`/`stopped`/`ended` | `{backend, state, waited_ms}` |
| `wait node --node <id> --tree-rev <rev> --state <state>` | Block until a node from a specific snapshot is `visible`/`hidden`/`focused`/`enabled` | `{node_id, tree_rev, state, waited_ms}` |
| `wait text --text <value>` | Block until text appears on screen or in UI tree | `{text, waited_ms}` |
| `wait text --text <value> --scope <screen\|ui>` | Scope text search | same |

All accept: `--timeout-ms <ms>` (default 10000), `--poll-ms <ms>` (default 250).

Node waits inherit the same stale-reference rules as `ui *`.

---

### 10. Volume

| Command | Does exactly | Output `data` |
|---|---|---|
| `volume get` | Read current volume | `{level, muted, max}` |
| `volume set <n>` | Set absolute level (0-100) | `{level, previous}` |
| `volume mute` | Mute | `{muted: true}` |
| `volume unmute` | Unmute | `{muted: false}` |

---

### 11. Source

| Command | Does exactly | Output `data` |
|---|---|---|
| `source list` | All input sources | `{sources: [{id, label, connected, active, device_name}]}` |
| `source get` | Active source | `{id, label, device_name}` |
| `source set <id>` | Switch source | `{id, previous}` |

---

### 12. Channel

| Command | Does exactly | Output `data` |
|---|---|---|
| `channel list` | All channels | `{channels: [{id, number, name, type}]}` |
| `channel get` | Currently tuned | `{id, number, name, type}` |
| `channel set <number\|id>` | Tune to channel | `{id, number, name, previous}` |

---

### 13. EPG (read-only)

| Command | Does exactly | Output `data` |
|---|---|---|
| `epg now` | What's on now, all channels | `{programs: [{channel_id, channel_name, title, start, end, rating}]}` |
| `epg now --channel <id>` | What's on now, one channel | `{channel_id, program: {title, start, end, description, rating}}` |
| `epg schedule --channel <id>` | Upcoming for one channel | `{channel_id, programs: [{title, start, end, description, rating}]}` |
| `epg schedule --channel <id> --hours <n>` | Next N hours (default 4) | same |

---

### 14. Settings

| Command | Does exactly | Output `data` |
|---|---|---|
| `setting get <key>` | Read one setting | `{key, value, type, range\|options}` |
| `setting set <key> <value>` | Write one setting | `{key, value, previous}` |
| `setting list` | All accessible settings | `{settings: [{key, value, type, writable}]}` |
| `setting list --category <cat>` | Filter: `picture`, `sound`, `network`, `general`, `accessibility` | same |

---

### 15. Power

| Command | Does exactly | Output `data` |
|---|---|---|
| `power status` | Current power state | `{state: "on"\|"standby"}` |
| `power standby` | Enter standby | `{state: "standby", previous}` |
| `power reboot` | Reboot | `{rebooting: true}` |

---

### 16. Notify

| Command | Does exactly | Output `data` |
|---|---|---|
| `notify show <title> <body>` | Display toast notification | `{id, title, body, duration_sec}` |
| `notify show <title> <body> --duration <sec>` | Custom duration (default 3) | same |
| `notify dismiss` | Dismiss current notification | `{dismissed: true}` |

---

### 17. Observe (streaming)

| Command | Does exactly | Output |
|---|---|---|
| `observe --topic <name>` | Stream state changes as NDJSON envelopes | One standard JSON envelope per event line |
| Topics: `app`, `playback`, `ui`, `state` | Repeatable `--topic` flag | `data = {topic, event, payload, seq}` per line |

Long-running. Agent reads stdout line-by-line. More efficient than polling for reactive behavior, and it can reuse the same envelope parser as single-shot commands.

An optional daemon/service is the natural implementation point for `observe`, but the CLI contract stays the same whether events are sourced directly or proxied through that daemon.

---

### 18. Pick (utility)

Built-in `jq`-lite for composability on constrained environments.

| Command | Does exactly | Output |
|---|---|---|
| `pick <path> --stdin` | Extract a value from stdin JSON | `{value}` |
| `pick <path> --stdin --raw` | Extract a value without the envelope | Raw scalar / JSON fragment |

```bash
tvpilot content search --query "dark knight" --limit 1 | tvpilot pick .data.results[0].launch.uri --stdin --raw
# stdout: netflix://watch/70143836
```

Path syntax: `.data.results[0].title`, `[*]` for all elements.

Default output stays machine-uniform; `--raw` is a shell convenience.

---

## Error Code Catalog

| Code | When | Retryable |
|------|------|-----------|
| `APP_NOT_FOUND` | App handle or app ID doesn't match any installed app | no |
| `AMBIGUOUS_APP_REF` | A handle or partial app reference resolves to multiple plausible installed apps | no |
| `APP_LAUNCH_FAILED` | App failed to launch | yes |
| `APP_NOT_RUNNING` | Tried to close an app that isn't running | no |
| `CONTENT_NOT_FOUND` | Content ID invalid or unavailable | no |
| `PROVIDER_UNAVAILABLE` | Provider app not installed | no |
| `AUTH_REQUIRED` | Provider app not logged in | no |
| `PLAYBACK_FAILED` | Playback could not start | yes |
| `PLAYBACK_INACTIVE` | Playback command but nothing playing | no |
| `ELEMENT_NOT_FOUND` | UI node ID not found in tree | no |
| `STALE_REFERENCE` | UI tree revision or node reference is stale | yes |
| `PRECONDITION_FAILED` | A guard such as `--expect-app` did not match current state | yes |
| `NAVIGATION_FAILED` | Key injection or focus move failed | yes |
| `SETTING_NOT_FOUND` | Unknown settings key | no |
| `SETTING_READ_ONLY` | Cannot write this setting | no |
| `SETTING_INVALID_VALUE` | Value out of range or wrong type | no |
| `CHANNEL_NOT_FOUND` | Channel doesn't exist | no |
| `SOURCE_NOT_AVAILABLE` | Input source not connected | no |
| `KEY_UNKNOWN` | Unrecognized key name | no |
| `PERMISSION_DENIED` | Lacks platform privilege | no |
| `INVALID_ARGS` | Bad arguments or malformed JSON | no |
| `CAPABILITY_UNAVAILABLE` | Requested provider/backend/capability is not available on this device/runtime | no |
| `UNSUPPORTED` | Not supported on this device/OS | no |
| `TIMEOUT` | Operation or wait timed out | yes |
| `PLATFORM_ERROR` | Tizen API returned unexpected error | yes |
| `STDIN_PARSE_ERROR` | Could not parse JSON from stdin | no |
| `PICK_PATH_ERROR` | JSON path doesn't resolve | no |

Each error includes: `code`, `msg`, `retryable`, `hint` (suggested next command).

---

## Full Command Summary

| # | Command | Domain | R/W |
|---|---------|--------|-----|
| 1 | `schema` | introspection | R |
| 2 | `capabilities` | introspection | R |
| 3 | `health` | introspection | R |
| 4 | `system info` | system | R |
| 5 | `system memory` | system | R |
| 6 | `system storage` | system | R |
| 7 | `system uptime` | system | R |
| 8 | `system network` | system | R |
| 9 | `app list` | app | R |
| 10 | `app info <app_ref>` | app | R |
| 11 | `app launch <app_ref>` | app | W |
| 12 | `app close <app_ref>` | app | W |
| 13 | `app foreground` | app | R |
| 14 | `app installed <app_ref>` | app | R |
| 15 | `app install <path>` | app | W |
| 16 | `app uninstall <app_ref>` | app | W |
| 17 | `inspect app` | inspect | R |
| 18 | `inspect screen` | inspect | R |
| 19 | `inspect playback` | inspect | R |
| 20 | `inspect ui` | inspect | R |
| 21 | `ui focus <node>` | ui | W |
| 22 | `ui activate <node>` | ui | W |
| 23 | `ui input <node>` | ui | W |
| 24 | `ui text --query <text>` | ui | R |
| 25 | `ui path` | ui | R |
| 26 | `nav press <key>` | nav | W |
| 27 | `nav text --value <text>` | nav | W |
| 28 | `nav home` | nav | W |
| 29 | `nav back` | nav | W |
| 30 | `nav keys` | nav | R |
| 31 | `content search` | content | R |
| 32 | `content discover` | content | R |
| 33 | `content resolve` | content | R |
| 34 | `content info <id>` | content | R |
| 35 | `content providers` | content | R |
| 36 | `play start` | playback | W |
| 37 | `play pause` | playback | W |
| 38 | `play resume` | playback | W |
| 39 | `play stop` | playback | W |
| 40 | `play seek` | playback | W |
| 41 | `play status` | playback | R |
| 42 | `wait app` | wait | U |
| 43 | `wait idle` | wait | U |
| 44 | `wait playback` | wait | U |
| 45 | `wait node` | wait | U |
| 46 | `wait text` | wait | U |
| 47 | `volume get` | volume | R |
| 48 | `volume set <n>` | volume | W |
| 49 | `volume mute` | volume | W |
| 50 | `volume unmute` | volume | W |
| 51 | `source list` | source | R |
| 52 | `source get` | source | R |
| 53 | `source set <id>` | source | W |
| 54 | `channel list` | channel | R |
| 55 | `channel get` | channel | R |
| 56 | `channel set <id>` | channel | W |
| 57 | `epg now` | epg | R |
| 58 | `epg schedule` | epg | R |
| 59 | `setting get <key>` | setting | R |
| 60 | `setting set <key> <val>` | setting | W |
| 61 | `setting list` | setting | R |
| 62 | `power status` | power | R |
| 63 | `power standby` | power | W |
| 64 | `power reboot` | power | W |
| 65 | `notify show` | notify | W |
| 66 | `notify dismiss` | notify | W |
| 67 | `observe` | stream | R |
| 68 | `pick <path> --stdin` | utility | U |

R = read, W = write, U = utility

---

## Agent Interaction Examples

### "Play Stranger Things on Netflix"
```bash
tvpilot content resolve --title "stranger things" --provider netflix | \
tvpilot app launch --stdin --path .data.launch && \
tvpilot wait idle && \
tvpilot volume set 30
```

### "What's on this screen?" (UI-aware agent)
```bash
tree=$(tvpilot inspect ui --visible-only --depth 3)
rev=$(echo "$tree" | tvpilot pick .data.tree_rev --stdin --raw)
app=$(echo "$tree" | tvpilot pick .data.app_handle --stdin --raw)
# Agent reads the tree, finds a search box node n-42

tvpilot ui input n-42 --tree-rev "$rev" --expect-app "$app" --value "cooking shows" --submit
tvpilot wait idle
tree=$(tvpilot inspect ui --visible-only)
rev=$(echo "$tree" | tvpilot pick .data.tree_rev --stdin --raw)
# Agent reads results, picks one

tvpilot ui activate n-87 --tree-rev "$rev" --expect-app "$app"
```

### "Navigate YouTube blindly" (no UI tree)
```bash
tvpilot app launch youtube && \
tvpilot wait app --app youtube --state foreground && \
tvpilot nav press UP UP LEFT ENTER && \
tvpilot nav text --value "cooking videos" --submit
```

### "Set up movie night"
```bash
tvpilot source set HDMI1 && \
tvpilot setting set picture.mode movie && \
tvpilot setting set picture.backlight 8 && \
tvpilot volume set 40 && \
tvpilot notify show "Movie Night" "Ready! Enjoy the show." --duration 5
```

### "Recover from unknown screen"
```bash
tvpilot inspect screen --base64    # vision agent looks at screenshot
tvpilot inspect ui --depth 2       # structural fallback
tvpilot ui path                    # where have we been?
tvpilot nav back --repeat 3        # bail out
tvpilot nav home                   # safe state
```

### "Wait for content to start, then adjust"
```bash
tvpilot play start --content-id netflix:series:80057281 --resume && \
tvpilot wait playback --state playing --timeout-ms 15000 && \
tvpilot volume set 25 && \
tvpilot play seek --delta-ms 60000   # skip 1 minute
```

---

## Reserved Future Namespaces

These are not specced yet but the names are reserved to avoid collisions:

- `recipe` — agent-authored reusable strategies (put/match/run/forget)
- `session` — stateful sessions for caching and continuity
- `bluetooth` — paired device management
- `tts` — text-to-speech on TV speakers
- `iot` — SmartThings-connected device control
- `voice` — voice input/output
- `account` — account state inspection
- `testing` — test automation helpers

---

## Open Questions

1. **UI tree availability** — Does the Tizen TV expose an accessibility tree (AT-SPI, EFL accessibility) that `inspect ui` can read? If not, the agent falls back to `inspect screen` + vision + `nav press`. This determines whether the `ui` namespace is feasible in Phase 2 or deferred.

2. **Screen capture foundation** — Can we provide `inspect screen` cheaply and reliably enough for Phase 1? If the UI tree is absent or partial, screenshot capture becomes the agent's primary perception path.

3. **Playback observation** — Can we observe playback state of 3rd-party apps (Netflix, YouTube)? Or can we only inject media keys? Determines whether `play status` and `wait playback` expose real state or stay capability-gated behind specific adapters.

4. **Content provider adapters** — How do we discover content from 3rd-party apps on-device? Options: (a) Samsung universal search API, (b) per-provider deep-link catalogs, (c) Tizen content framework. Determines feasibility of `content search/resolve`.

5. **Field filtering scope** — Should `--field` work as a JSON path filter (like GraphQL field selection) or simple top-level key inclusion? Former is more useful, latter is simpler to implement.

---

## Phasing

### Phase 1 — Foundation
`schema`, `capabilities`, `health`, `system *`, `app *`, `inspect screen`, `nav *`, `volume *`, `power *`, `wait app`, `wait idle`, `pick`

### Phase 2 — Structured Observation + Guarded UI
`inspect app`, `inspect ui`, snapshot guards (`tree_rev`, `--expect-app`), `ui *`, `wait node`, `wait text`, `--field` filtering

### Phase 3 — Adapter-Backed Media + Content
`inspect playback`, `content *`, `play *`, `wait playback`, provider/backend capability maps

### Phase 4 — TV Control
`source *`, `channel *`, `epg *`, `setting *`, `notify *`, `app install/uninstall`

### Phase 5 — Efficiency + Learning
`observe`, `session *`, `recipe *`, confidence tagging, streaming NDJSON envelopes
