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
2. **Agent-first** — No interactive prompts, no color, no spinners. Pure function: args in, JSON out.
3. **Self-describing** — `schema`, `capabilities`, and `health` let the agent discover what is available and ready at runtime.
4. **Pipe-native** — stdout is always single-line JSON. Commands can read prior output from stdin via `--stdin`. One invocation = one line.
5. **Idempotent where possible** — Repeated calls produce the same result or a no-op.
6. **Fail loud, fail structured** — Errors are typed JSON with machine-readable codes, retry hints, and suggested next actions.
7. **Stable IDs** — Apps, UI nodes, content items, and media state use durable references the agent can hold across calls.
8. **Forward-compatible** — New capabilities appear as new commands. The envelope never changes shape.

---

## I/O Contract

### Output Envelope (stdout, always JSON, one line)

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

### Exit Codes

| Code | Meaning |
|------|---------|
| 0    | Success (`ok: true`) |
| 1    | Command failed (`ok: false`, see `error.code`) |
| 2    | Usage error (bad args, unknown subcommand) |

### Input Conventions

- Positional args for the primary target: `tvpilot app launch org.tizen.netflix`
- Flags for modifiers: `--limit 5`, `--provider netflix`
- `--stdin`: read JSON from stdin (for piping previous command output)
- `--request-id <id>`: caller-generated correlation ID, echoed back in `rid`
- `--timeout-ms <ms>`: per-command timeout override
- `--field <path>`: repeated flag to request partial response (reduces token cost)
- Long flags only — agents don't need brevity, they need clarity

### Composability Patterns

```bash
# Pipe: extract from previous output
tvpilot content search "dark knight" --limit 1 | tvpilot pick .data.results[0].deep_link.uri --stdin

# Chain: sequential with exit-code gating
tvpilot app launch org.tizen.netflix --uri "netflix://watch/70143836" && tvpilot wait idle && tvpilot volume set 20

# Subshell capture
tvpilot app launch org.tizen.netflix --uri "$(tvpilot content resolve --title "dark knight" | tvpilot pick .data.launch_uri --stdin)"

# Parallel
tvpilot app foreground & tvpilot volume get & tvpilot play status & wait

# Script
result=$(tvpilot content search "stranger things" --provider netflix --limit 1)
uri=$(echo "$result" | tvpilot pick .data.results[0].deep_link.uri --stdin)
app=$(echo "$result" | tvpilot pick .data.results[0].deep_link.app_id --stdin)
tvpilot app launch "$app" --uri "$uri"
```

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

`health` is a pre-flight check: are Tizen APIs accessible, is the automation backend up, can we inject keys.

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
| `app list` | All installed apps | `{apps: [{app_id, name, version, type}]}` |
| `app list --running` | Only running apps | `{apps: [{app_id, name, pid, state}]}` |
| `app info <app_id>` | One app's metadata | `{app_id, name, version, type, state, installed_size_kb}` |
| `app launch <app_id>` | Launch/foreground an app | `{app_id, pid}` |
| `app launch <app_id> --uri <deeplink>` | Launch with deep-link | `{app_id, pid, uri}` |
| `app launch <app_id> --extra key=val` | Launch with app_control extras (repeatable) | `{app_id, pid, extras: {k:v}}` |
| `app close <app_id>` | Terminate a running app | `{app_id, was_running}` |
| `app foreground` | Which app is currently focused | `{app_id, name, pid}` |
| `app installed <app_id>` | Check if an app is installed (exits 0/1) | `{app_id, installed: bool}` |
| `app install <path>` | Install .wgt/.tpk from local path | `{app_id, version}` |
| `app uninstall <app_id>` | Remove an app | `{app_id}` |

---

### 4. Inspect

Observation-only commands for reading screen and UI state. Separated from mutation commands.

| Command | Does exactly | Output `data` |
|---|---|---|
| `inspect app` | Current foreground app | `{app_id, name, pid}` |
| `inspect screen` | Capture current screen | `{path, width, height, size_bytes}` |
| `inspect screen --base64` | Capture as inline base64 | `{width, height, encoding, image}` |
| `inspect screen --region <x,y,w,h>` | Capture a region | same |
| `inspect playback` | Current playback state | `{state, content_id, title, position_ms, duration_ms, speed, audio_track, subtitle}` |
| `inspect ui` | Machine-readable accessibility/UI tree | `{root: {node_id, role, label, ...children}}` |
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

This is what enables the agent to reason about screen structure instead of blindly pressing keys.

---

### 5. UI

Mutation commands that interact with UI elements by node ID (obtained from `inspect ui`).

| Command | Does exactly | Output `data` |
|---|---|---|
| `ui focus <node_id>` | Move focus to a node | `{node_id, focused: true}` |
| `ui activate <node_id>` | Activate a node (click/select) | `{node_id, action: "activate"}` |
| `ui activate <node_id> --action <name>` | Specific action: `open`, `play`, `select` | same |
| `ui input <node_id> --value <text>` | Type text into a specific field node | `{node_id, value}` |
| `ui input <node_id> --value <text> --submit` | Type and submit | same |
| `ui text --query <text>` | Find visible nodes by text match | `{matches: [{node_id, role, label, text, bounds}]}` |
| `ui text --query <text> --exact` | Exact match only | same |
| `ui path` | Focus/back navigation history | `{path: [{node_id, label, role}]}` |

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

| Command | Does exactly | Output `data` |
|---|---|---|
| `content search --query <text>` | Free-text search across providers | `{query, results: [{content_id, title, type, provider, year, rating, synopsis, deep_link: {app_id, uri}, confidence}]}` |
| `content search --query <text> --provider <id>` | Search one provider | same |
| `content search --query <text> --type <movie\|series\|live>` | Filter by type | same |
| `content search --query <text> --limit <n>` | Cap results (default 10) | same |
| `content search --query <text> --cursor <cursor>` | Pagination | same + `{cursor}` |
| `content discover --category <name>` | Browse: `trending`, `recommended`, `continue_watching`, `new` | `{category, items: [{content_id, title, ...}]}` |
| `content discover --category <name> --provider <id>` | Category within one provider | same |
| `content resolve --title <title>` | Turn a title into a launchable deep-link | `{content_id, title, provider, launch_uri, app_id, confidence}` |
| `content resolve --content-id <id>` | Resolve by ID | same |
| `content info <content_id>` | Full metadata | `{content_id, title, type, provider, year, rating, synopsis, seasons, episodes, cast, deep_link}` |
| `content providers` | List available providers | `{providers: [{id, app_id, installed, searchable, authenticated}]}` |

`content resolve` is the critical bridge: "I know the title" → "here's the app_id and URI to launch it." It can return a `confidence` score so the agent knows whether to trust or verify.

---

### 8. Playback

Dedicated playback control with structured state. More reliable than key injection for media control.

| Command | Does exactly | Output `data` |
|---|---|---|
| `play start --content-id <id>` | Start playing content | `{content_id, state}` |
| `play start --launch-uri <uri>` | Start via deep-link URI | `{launch_uri, state}` |
| `play start ... --resume` | Resume from last position | same |
| `play pause` | Pause | `{state: "paused", position_ms}` |
| `play resume` | Resume | `{state: "playing", position_ms}` |
| `play stop` | Stop | `{state: "stopped"}` |
| `play seek --position-ms <ms>` | Seek to absolute position | `{position_ms}` |
| `play seek --delta-ms <ms>` | Seek relative (positive=forward, negative=back) | `{position_ms, delta_ms}` |
| `play status` | Current playback state | `{state, content_id, title, position_ms, duration_ms, progress_pct, speed, audio_track, subtitle}` |

---

### 9. Wait

Blocking primitives so the agent doesn't write poll loops. Each one blocks until a condition is met or `--timeout-ms` is exceeded.

| Command | Does exactly | Output `data` |
|---|---|---|
| `wait app --app-id <id> --state <state>` | Block until app reaches `foreground`/`background`/`closed`/`ready` | `{app_id, state, waited_ms}` |
| `wait idle` | Block until screen is stable (no transitions) | `{waited_ms}` |
| `wait playback --state <state>` | Block until playback reaches `playing`/`paused`/`stopped`/`ended` | `{state, waited_ms}` |
| `wait node --node <id> --state <state>` | Block until UI node is `visible`/`hidden`/`focused`/`enabled` | `{node_id, state, waited_ms}` |
| `wait text --text <value>` | Block until text appears on screen or in UI tree | `{text, waited_ms}` |
| `wait text --text <value> --scope <screen\|ui>` | Scope text search | same |

All accept: `--timeout-ms <ms>` (default 10000), `--poll-ms <ms>` (default 250).

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
| `observe --topic <name>` | Stream state changes as NDJSON | One JSON line per event |
| Topics: `app`, `playback`, `ui`, `state` | Repeatable `--topic` flag | `{topic, event, data, ts}` per line |

Long-running. Agent reads stdout line-by-line. More efficient than polling for reactive behavior.

---

### 18. Pick (utility)

Built-in `jq`-lite for composability on constrained environments.

| Command | Does exactly | Output |
|---|---|---|
| `pick <path> --stdin` | Extract a value from stdin JSON | **Raw value** — no envelope |

```bash
tvpilot content search --query "dark knight" --limit 1 | tvpilot pick .data.results[0].deep_link.uri --stdin
# stdout: netflix://watch/70143836
```

Path syntax: `.data.results[0].title`, `[*]` for all elements.

---

## Error Code Catalog

| Code | When | Retryable |
|------|------|-----------|
| `APP_NOT_FOUND` | App ID doesn't match any installed app | no |
| `APP_LAUNCH_FAILED` | App failed to launch | yes |
| `APP_NOT_RUNNING` | Tried to close an app that isn't running | no |
| `CONTENT_NOT_FOUND` | Content ID invalid or unavailable | no |
| `PROVIDER_UNAVAILABLE` | Provider app not installed | no |
| `AUTH_REQUIRED` | Provider app not logged in | no |
| `PLAYBACK_FAILED` | Playback could not start | yes |
| `PLAYBACK_INACTIVE` | Playback command but nothing playing | no |
| `ELEMENT_NOT_FOUND` | UI node ID not found in tree | no |
| `NAVIGATION_FAILED` | Key injection or focus move failed | yes |
| `SETTING_NOT_FOUND` | Unknown settings key | no |
| `SETTING_READ_ONLY` | Cannot write this setting | no |
| `SETTING_INVALID_VALUE` | Value out of range or wrong type | no |
| `CHANNEL_NOT_FOUND` | Channel doesn't exist | no |
| `SOURCE_NOT_AVAILABLE` | Input source not connected | no |
| `KEY_UNKNOWN` | Unrecognized key name | no |
| `PERMISSION_DENIED` | Lacks platform privilege | no |
| `INVALID_ARGS` | Bad arguments or malformed JSON | no |
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
| 10 | `app info <id>` | app | R |
| 11 | `app launch <id>` | app | W |
| 12 | `app close <id>` | app | W |
| 13 | `app foreground` | app | R |
| 14 | `app installed <id>` | app | R |
| 15 | `app install <path>` | app | W |
| 16 | `app uninstall <id>` | app | W |
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
tvpilot content resolve --title "stranger things" --provider netflix
# → {content_id, app_id, launch_uri, confidence: 0.95}

tvpilot app launch org.tizen.netflix --uri "netflix://watch/80057281" && \
tvpilot wait idle && \
tvpilot volume set 30
```

### "What's on this screen?" (UI-aware agent)
```bash
tvpilot inspect ui --visible-only --depth 3
# Agent reads the tree, finds a search box

tvpilot ui input n-42 --value "cooking shows" --submit
tvpilot wait idle
tvpilot inspect ui --visible-only
# Agent reads results, picks one

tvpilot ui activate n-87
```

### "Navigate YouTube blindly" (no UI tree)
```bash
tvpilot app launch org.tizen.youtube && \
tvpilot wait app --app-id org.tizen.youtube --state foreground && \
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

1. **UI tree availability** — Does the Tizen TV expose an accessibility tree (AT-SPI, EFL accessibility) that `inspect ui` can read? If not, the agent falls back to `inspect screen` + vision + `nav press`. This determines whether the `ui` namespace is feasible in Phase 1 or deferred.

2. **Playback observation** — Can we observe playback state of 3rd-party apps (Netflix, YouTube)? Or can we only inject media keys? Determines whether `play status` and `wait playback` return real data for 3rd-party content.

3. **Content provider adapters** — How do we discover content from 3rd-party apps on-device? Options: (a) Samsung universal search API, (b) per-provider deep-link catalogs, (c) Tizen content framework. Determines feasibility of `content search/resolve`.

4. **`jq` availability** — Is `jq` on the TV filesystem? If not, `pick` is critical infrastructure. If yes, it's a convenience.

5. **Field filtering scope** — Should `--field` work as a JSON path filter (like GraphQL field selection) or simple top-level key inclusion? Former is more useful, latter is simpler to implement.

---

## Phasing

### Phase 1 — Foundation
`schema`, `capabilities`, `health`, `system *`, `app *`, `nav *`, `volume *`, `power *`, `wait app`, `wait idle`, `pick`

### Phase 2 — Observation + UI + Content
`inspect *`, `ui *`, `content *`, `play *`, `wait node`, `wait text`, `wait playback`, `--field` filtering

### Phase 3 — TV Control
`source *`, `channel *`, `epg *`, `setting *`, `notify *`, `app install/uninstall`

### Phase 4 — Efficiency + Learning
`observe`, `session *`, `recipe *`, confidence tagging, streaming NDJSON
