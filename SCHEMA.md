# tvpilot Core Schema

This file defines the current public chain:

- `search sources`
- `content search`
- `play`

In this phase:

- a `source` is a search tool
- a `catalog` is a logical content namespace like `youtube`, `netflix`, or `watcha`
- `content search` filters sources by `sources[]`, `catalog`, and `query`
- if no source filter is provided, `content search` queries all matching available sources, aggregates the results, and dedupes them

---

## Shared Envelope

Every one-shot command returns the same top-level envelope:

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
  "ms": 7,
  "v": "0.1.0"
}
```

---

## Shared Types

### SearchSource

Describes one search tool the agent can choose.

```jsonc
{
  "id": "searchon",
  "display_name": "Samsung Search",
  "available": true,
  "catalogs": ["tv_plus", "youtube", "watcha", "netflix"]
}
```

Field meanings:

- `id`: canonical source id used by `content search --source`
- `display_name`: human-facing source label
- `available`: whether the source can currently be queried
- `catalogs`: logical catalogs this source can search

Selection rule:

- If a source does not list a catalog, the agent must not use that source to search that catalog.

### LaunchSpec

Normalized launch object produced by `content search` and consumable by `play`.

```jsonc
{
  "app_handle": "netflix",
  "app_display_name": "Netflix",
  "app_id": "org.netflix",
  "launch_uri": "netflix://watch/80057281",
  "launch_extras": {},
  "launch_method": "deeplink"
}
```

Field meanings:

- `app_handle`: canonical app/service handle used by the CLI
- `app_display_name`: human-facing app label
- `app_id`: exact platform-native target identifier
- `launch_uri`: deep-link or launch URI when one exists
- `launch_extras`: structured launch extras when supported
- `launch_method`: how the target is opened, for example `deeplink`, `app_control`, or `native`

### ContentSearchResult

```jsonc
{
  "content_id": "netflix:movie:12345",
  "dedupe_key": "imdb:tt0092099",
  "title": "Top Gun",
  "type": "movie",
  "source": "searchon",
  "source_display_name": "Samsung Search",
  "catalog": "netflix",
  "catalog_display_name": "Netflix",
  "year": 1986,
  "rating": "PG",
  "synopsis": "A young naval aviator competes at an elite fighter school.",
  "launchable": true,
  "auth_required": false,
  "launch": {
    "app_handle": "netflix",
    "app_display_name": "Netflix",
    "app_id": "org.netflix",
    "launch_uri": "netflix://watch/12345",
    "launch_extras": {},
    "launch_method": "deeplink"
  },
  "confidence": 0.97
}
```

Field meanings:

- `source`: which search tool produced the result
- `catalog`: which logical catalog the result belongs to
- `launch`: the `LaunchSpec` that `play` can consume directly
- `confidence`: normalized match confidence

Important distinction:

- `source` is the queried tool
- `catalog` is the content namespace in the result
- one source may return many catalogs
- the same catalog may appear from multiple sources

### PlayResult

Returned by `play` after it accepts a `LaunchSpec`.

```jsonc
{
  "state": "starting",
  "app_handle": "netflix",
  "app_display_name": "Netflix",
  "app_id": "org.netflix",
  "launch_uri": "netflix://watch/12345",
  "launch_method": "deeplink"
}
```

Field meanings:

- `state`: current acknowledgement state, currently `starting`
- `app_handle`, `app_display_name`, `app_id`: resolved launch target
- `launch_uri`, `launch_method`: launch route that was used

---

## Command Schemas

### 1. `search sources`

CLI:

```bash
tvpilot search sources
```

`data` shape:

```jsonc
{
  "sources": [
    {
      "id": "searchon",
      "display_name": "Samsung Search",
      "available": true,
      "catalogs": ["tv_plus", "youtube", "watcha", "netflix"]
    },
    {
      "id": "youtube_direct",
      "display_name": "YouTube",
      "available": true,
      "catalogs": ["youtube"]
    },
    {
      "id": "watcha_direct",
      "display_name": "Watcha",
      "available": false,
      "catalogs": ["watcha"]
    }
  ]
}
```

This output is the only source-planning spec the agent needs.

### 2. `content search`

CLI:

```bash
tvpilot content search --query "top gun"
tvpilot content search --query "top gun" --catalog netflix
tvpilot content search --query "top gun" --source searchon
tvpilot content search --query "lofi hip hop" --catalog youtube --source searchon --source youtube_direct
```

Conceptual input:

```jsonc
{
  "cmd": "content.search",
  "args": {
    "query": "top gun",
    "sources": ["searchon"],
    "catalog": "netflix",
    "limit": 5,
    "cursor": null
  }
}
```

`data` shape:

```jsonc
{
  "query": "top gun",
  "sources": ["searchon"],
  "catalog": "netflix",
  "results": [
    {
      "content_id": "netflix:movie:12345",
      "dedupe_key": "imdb:tt0092099",
      "title": "Top Gun",
      "type": "movie",
      "source": "searchon",
      "source_display_name": "Samsung Search",
      "catalog": "netflix",
      "catalog_display_name": "Netflix",
      "year": 1986,
      "rating": "PG",
      "synopsis": "A young naval aviator competes at an elite fighter school.",
      "launchable": true,
      "auth_required": false,
      "launch": {
        "app_handle": "netflix",
        "app_display_name": "Netflix",
        "app_id": "org.netflix",
        "launch_uri": "netflix://watch/12345",
        "launch_extras": {},
        "launch_method": "deeplink"
      },
      "confidence": 0.97
    }
  ],
  "cursor": null
}
```

Search semantics:

- `query` is required
- `source` is repeatable and pins the exact source list when provided
- `catalog` filters sources and results by catalog
- if `source` is omitted, the CLI starts from all available sources
- if `catalog` is provided, the CLI keeps only sources whose `catalogs` include that catalog
- the CLI queries the remaining sources, aggregates results, and dedupes overlaps
- if no source remains after filtering, return `NO_MATCHING_SOURCE`

Selection examples:

- `--catalog netflix` means use only sources that advertise `netflix`
- `--source watcha_direct --catalog netflix` should fail with `NO_MATCHING_SOURCE`
- no `--source` and no `--catalog` means search all available sources

### 3. `play`

CLI:

```bash
tvpilot play --launch-spec '{"app_handle":"netflix","app_display_name":"Netflix","app_id":"org.netflix","launch_uri":"netflix://watch/12345","launch_extras":{},"launch_method":"deeplink"}'
tvpilot play --stdin
tvpilot play --stdin --path .data.results[0].launch
```

Conceptual input:

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

`data` shape:

```jsonc
{
  "state": "starting",
  "app_handle": "netflix",
  "app_display_name": "Netflix",
  "app_id": "org.netflix",
  "launch_uri": "netflix://watch/12345",
  "launch_method": "deeplink"
}
```

Execution notes:

- `play` accepts either `--launch-spec <json>` or `--stdin`
- when stdin contains a larger envelope, `--path` extracts the nested `LaunchSpec`
- `play` only needs `LaunchSpec`; it does not need title metadata

---

## Chaining Examples

### 1. Inspect Search Tools

```bash
tvpilot search sources
```

### 2. Search Every Available Source

```bash
tvpilot content search --query "top gun"
```

### 3. Search Every Netflix-Capable Source

```bash
tvpilot content search --query "top gun" --catalog netflix
```

### 4. Search One Explicit Source

```bash
tvpilot content search --query "top gun" --source searchon
```

### 5. Search Two Explicit Sources For One Catalog

```bash
tvpilot content search \
  --query "lofi hip hop" \
  --catalog youtube \
  --source searchon \
  --source youtube_direct
```

### 6. Search Then Play First Result

```bash
tvpilot content search --query "top gun" --catalog netflix --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### 7. Search All Sources Then Play First Result

```bash
tvpilot content search --query "top gun" --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### 8. Search One Source Then Play

```bash
tvpilot content search --query "top gun" --source searchon --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### 9. Search One Catalog Across Multiple Sources Then Play

```bash
tvpilot content search \
  --query "lofi hip hop" \
  --catalog youtube \
  --source searchon \
  --source youtube_direct \
  --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### 10. Play From An Inline LaunchSpec

```bash
tvpilot play --launch-spec '{"app_handle":"youtube","app_display_name":"YouTube","app_id":"org.youtube","launch_uri":"youtube://watch?v=abc123","launch_extras":{},"launch_method":"deeplink"}'
```
