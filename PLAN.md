# tvpilot — Reduced Scope Plan

Current public chain:

- `search sources`
- `content search`
- `play`

This phase is intentionally only about search-tool discovery, content search, and launch handoff.

---

## Core Philosophy

`tvpilot` should expose only the minimum surfaces needed for an agent to:

1. discover which search tools exist
2. filter those tools by catalog and explicit source choice
3. search content
4. hand a launch object into playback

Design rules:

1. A `source` is a search tool.
2. A `catalog` is the content namespace a source can search.
3. `search sources` returns the source spec the agent plans from.
4. `content search` accepts `query`, optional `sources[]`, and optional `catalog`.
5. If `sources[]` is omitted, `content search` queries all matching available sources.
6. `play` accepts a `LaunchSpec` directly or from stdin.

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
    "code": "NO_MATCHING_SOURCE",
    "msg": "No available source can satisfy the requested filters",
    "retryable": false,
    "hint": "search sources"
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
- Use `--source <id>` as a repeatable filter when the caller wants specific search tools.
- Use `--catalog <handle>` to keep only sources that can search that catalog.
- Use `--stdin` when a command should consume JSON from a prior command.
- Use `--path <json_path>` to extract a nested object from stdin before command execution.
- Use `--launch-spec <json>` when the caller wants to pass a launch object directly.

---

## Domain Model

### Source

A `source` is the search tool the CLI can query.

Examples:

- `searchon`
- `youtube_direct`
- `watcha_direct`

What the agent needs to know about a source:

- its id
- its display name
- whether it is available
- which catalogs it can search

### Catalog

A `catalog` is the logical content namespace being searched.

Examples:

- `youtube`
- `netflix`
- `watcha`
- `tv_plus`

This is just a filter for source selection and result labeling.

### Source Spec

`search sources` should return:

- `id`
- `display_name`
- `available`
- `catalogs`

That is enough for chaining.

Example interpretation:

- if a source lists `catalogs: ["youtube", "netflix"]`, the agent may use it for either catalog
- if a source lists only `["watcha"]`, the agent must not use it for `netflix`
- if two sources both list `youtube`, the agent may query both when the request needs YouTube results

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

### 1. Search Sources

| Command | Does exactly | Output `data` |
|---|---|---|
| `search sources` | List search tools and the catalogs each tool can search | `{sources: [{id, display_name, available, catalogs}]}` |

### 2. Content Search

| Command | Does exactly | Output `data` |
|---|---|---|
| `content search --query <text>` | Search all matching available sources | `{query, sources, catalog, results, cursor}` |
| `content search --query <text> --source <id>` | Search one explicit source | same |
| `content search --query <text> --source <id> --source <id>` | Search an explicit source list | same |
| `content search --query <text> --catalog <handle>` | Search only sources that can search that catalog | same |
| `content search --query <text> --catalog <handle> --source <id>...` | Search the intersection of source filter and catalog filter | same |
| `content search --query <text> --limit <n>` | Cap result count | same |
| `content search --query <text> --cursor <cursor>` | Fetch the next page | same plus `cursor` |

Search behavior:

1. `query` is required.
2. If `sources[]` is present, use exactly that source set.
3. Otherwise, start from all available sources.
4. If `catalog` is present, keep only sources whose `catalogs` include it.
5. Query the remaining sources.
6. Aggregate results and dedupe overlaps.
7. If no source remains after filtering, return `NO_MATCHING_SOURCE`.

### 3. Play

| Command | Does exactly | Output `data` |
|---|---|---|
| `play --launch-spec <json>` | Play from an inline `LaunchSpec` | `{state, app_handle, app_display_name, app_id, launch_uri, launch_method}` |
| `play --stdin` | Play from a raw `LaunchSpec` read from stdin | same |
| `play --stdin --path <path>` | Play from a nested `LaunchSpec` inside stdin JSON | same |

---

## Chaining Model

The intended chain is:

1. Inspect sources with `search sources`.
2. Decide whether the query needs all sources or a filtered source set.
3. Run `content search`.
4. Pipe the selected result's `launch` object into `play`.

Examples:

### Inspect tools

```bash
tvpilot search sources
```

### Search all tools

```bash
tvpilot content search --query "top gun"
```

### Search one catalog

```bash
tvpilot content search --query "top gun" --catalog netflix
```

### Search one source

```bash
tvpilot content search --query "top gun" --source searchon
```

### Search multiple explicit sources

```bash
tvpilot content search \
  --query "lofi hip hop" \
  --source searchon \
  --source youtube_direct
```

### Search one catalog across multiple sources

```bash
tvpilot content search \
  --query "lofi hip hop" \
  --catalog youtube \
  --source searchon \
  --source youtube_direct
```

### Search then play

```bash
tvpilot content search --query "top gun" --catalog netflix --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### Search all tools then play

```bash
tvpilot content search --query "top gun" --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### Search one source then play

```bash
tvpilot content search --query "top gun" --source searchon --limit 1 | \
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
| `SEARCH_SOURCE_UNAVAILABLE` | A requested source is unavailable or not queryable | no |
| `NO_MATCHING_SOURCE` | No available source matches the requested filters | no |
| `CONTENT_NOT_FOUND` | No content matched the request | no |
| `AUTH_REQUIRED` | The result is known but playback requires authentication | no |
| `INVALID_LAUNCH_SPEC` | `play` did not receive a valid `LaunchSpec` | no |
| `PLAYBACK_UNAVAILABLE` | Playback could not be started for the requested launch target | yes |

---

## Out of Scope

Not part of this phase:

- app inventory or app launch commands outside `play`
- playback status or control families beyond `play`
- UI tree inspection or UI interaction
- low-level navigation
- system control such as volume, settings, channel, or power
