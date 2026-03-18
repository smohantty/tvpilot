# tvpilot — Reduced Scope Plan

Current public chain:

- `search catalogs`
- `content search`
- `play`

This phase is intentionally only about catalog discovery, content search, and launch handoff.

---

## Core Philosophy

`tvpilot` should expose only the minimum surfaces needed for an agent to:

1. discover which catalogs are searchable
2. optionally constrain search to one catalog
3. search content
4. hand a launch object into playback

Design rules:

1. `search catalogs` returns only a list of searchable catalogs.
2. `content search` accepts `query` and optional `catalog`.
3. Internal search-tool selection is hidden behind `content search`.
4. If `catalog` is omitted, `content search` searches all searchable catalogs through the available internal tools.
5. `play` accepts a `LaunchSpec` directly or from stdin.

---

## I/O Contract

### Output Envelope

Every one-shot command emits exactly one JSON envelope on stdout.

```jsonc
{
  "ok": true,
  "cmd": "content.search",
  "rid": "req-42",
  "data": {},
  "error": null,
  "ts": "2026-03-18T08:21:00Z",
  "ms": 12,
  "v": "0.1.0"
}
```

Error form:

```jsonc
{
  "ok": false,
  "cmd": "content.search",
  "rid": "req-42",
  "data": null,
  "error": {
    "code": "CATALOG_UNSEARCHABLE",
    "msg": "No internal search tool can search catalog 'watcha'",
    "retryable": false,
    "hint": "search catalogs"
  },
  "ts": "2026-03-18T08:21:00Z",
  "ms": 5,
  "v": "0.1.0"
}
```

### Exit Codes

| Code | Meaning |
|---|---|
| `0` | Command executed successfully |
| `1` | Command failed operationally and returned `ok: false` |
| `2` | Usage error such as bad flags or malformed arguments |

### Input Conventions

- Use `--query <text>` for the free-text search string.
- Use `--catalog <handle>` to constrain search to one catalog.
- Use `--stdin` when a command should consume JSON from a prior command.
- Use `--path <json_path>` to extract a nested object from stdin before command execution.
- Use `--launch-spec <json>` when the caller wants to pass a launch object directly.

---

## Domain Model

### Catalog

A `catalog` is the public planning token.

Examples:

- `youtube`
- `netflix`
- `watcha`
- `tv_plus`

The agent uses catalogs to decide whether to search broadly or constrain the query.

### Internal Source Selection

Internal search tools may exist, but they are not part of the public chain.

Rules:

- `search catalogs` exposes only what is searchable now
- `content search` chooses the needed internal search tools automatically
- if `catalog` is given, `content search` uses only tools that can search that catalog
- if `catalog` is omitted, `content search` searches across all searchable catalogs and aggregates results

### LaunchSpec

`play` consumes a normalized `LaunchSpec`:

```jsonc
{
  "app_handle": "netflix",
  "app_display_name": "Netflix",
  "app_id": "org.netflix",
  "launch_uri": "netflix://watch/12345",
  "launch_extras": {},
  "launch_method": "deeplink"
}
```

This object is execution-only. Search owns title and catalog metadata; `play` only needs launch data.

---

## Command Reference

### 1. Search Catalogs

| Command | Does exactly | Output `data` |
|---|---|---|
| `search catalogs` | List currently searchable catalogs | `{catalogs: [<handle>, ...]}` |

Example:

```jsonc
{
  "catalogs": ["netflix", "tv_plus", "watcha", "youtube"]
}
```

### 2. Content Search

| Command | Does exactly | Output `data` |
|---|---|---|
| `content search --query <text>` | Search across everything currently searchable | `{query, catalog, results, cursor}` |
| `content search --query <text> --catalog <handle>` | Search one catalog | same |
| `content search --query <text> --limit <n>` | Cap result count | same |
| `content search --query <text> --cursor <cursor>` | Fetch the next page | same plus `cursor` |

Search behavior:

1. `query` is required.
2. If `catalog` is omitted, search everything currently searchable.
3. If `catalog` is present, search only that catalog.
4. Internal search-tool selection and aggregation are handled by `tvpilot`.
5. If the requested catalog is not searchable, return `CATALOG_UNSEARCHABLE`.

### 3. Play

| Command | Does exactly | Output `data` |
|---|---|---|
| `play --launch-spec <json>` | Play from an inline `LaunchSpec` | `{state, app_handle, app_display_name, app_id, launch_uri, launch_method}` |
| `play --stdin` | Play from a raw `LaunchSpec` read from stdin | same |
| `play --stdin --path <path>` | Play from a nested `LaunchSpec` inside stdin JSON | same |

---

## Chaining Model

The intended chain is:

1. Inspect searchable catalogs with `search catalogs`.
2. Run `content search` with or without `--catalog`.
3. Pipe the selected result's `launch` object into `play`.

Examples:

### List searchable catalogs

```bash
tvpilot search catalogs
```

### Search across everything searchable

```bash
tvpilot content search --query "top gun"
```

### Search one catalog

```bash
tvpilot content search --query "top gun" --catalog netflix
```

### Search YouTube then play

```bash
tvpilot content search --query "lofi hip hop" --catalog youtube --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### Search Netflix then play

```bash
tvpilot content search --query "top gun" --catalog netflix --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### Search everything then play

```bash
tvpilot content search --query "top gun" --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### Play from an inline launch object

```bash
tvpilot play --launch-spec '{"app_handle":"youtube","app_display_name":"YouTube","app_id":"org.youtube","launch_uri":"youtube://watch?v=abc123","launch_extras":{},"launch_method":"deeplink"}'
```

---

## Error Code Catalog

| Code | When | Retryable |
|---|---|---|
| `CATALOG_UNSEARCHABLE` | The requested catalog is not currently searchable | no |
| `CONTENT_NOT_FOUND` | No content matched the request | no |
| `AUTH_REQUIRED` | The result is known but playback requires authentication | no |
| `INVALID_LAUNCH_SPEC` | `play` did not receive a valid `LaunchSpec` | no |
| `PLAYBACK_UNAVAILABLE` | Playback could not be started for the requested launch target | yes |

---

## Out of Scope

Not part of this phase:

- public source/tool selection
- app inventory or app launch commands outside `play`
- playback status or control families beyond `play`
- UI tree inspection or UI interaction
- low-level navigation
- system control such as volume, settings, channel, or power
