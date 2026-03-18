# tvpilot Core Schema

This file defines the current public chain:

- `search catalogs`
- `content search`
- `play`

In this phase:

- a `catalog` is a logical content namespace like `youtube`, `netflix`, or `watcha`
- `search catalogs` returns the currently searchable catalogs
- `content search` accepts `query` and optional `catalog`
- if no catalog is provided, `content search` searches all internal search tools, aggregates results, and dedupes overlaps
- internal source/tool selection is not part of the public contract

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
    "code": "CATALOG_UNSEARCHABLE",
    "msg": "No internal search tool can search catalog 'watcha'",
    "retryable": false,
    "hint": "search catalogs"
  },
  "ts": "2026-03-18T08:21:00Z",
  "ms": 7,
  "v": "0.1.0"
}
```

---

## Shared Types

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

- `catalog`: the logical catalog the result belongs to
- `launch`: the `LaunchSpec` that `play` can consume directly
- `confidence`: normalized match confidence

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

### 1. `search catalogs`

CLI:

```bash
tvpilot search catalogs
```

`data` shape:

```jsonc
{
  "catalogs": ["netflix", "tv_plus", "watcha", "youtube"]
}
```

This list should contain only catalogs that are currently searchable.

### 2. `content search`

CLI:

```bash
tvpilot content search --query "top gun"
tvpilot content search --query "top gun" --catalog netflix
tvpilot content search --query "lofi hip hop" --catalog youtube
```

Conceptual input:

```jsonc
{
  "cmd": "content.search",
  "args": {
    "query": "top gun",
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
  "catalog": "netflix",
  "results": [
    {
      "content_id": "netflix:movie:12345",
      "dedupe_key": "imdb:tt0092099",
      "title": "Top Gun",
      "type": "movie",
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
- `catalog` is optional
- if `catalog` is omitted, the CLI searches all internally available search tools, aggregates results, and dedupes overlaps
- if `catalog` is provided, the CLI uses only internal search tools that can search that catalog
- if no internal search tool can search the requested catalog, return `CATALOG_UNSEARCHABLE`
- `launch` is the execution handoff object for `play`

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
- `play` only needs `LaunchSpec`

---

## Chaining Examples

### 1. List Searchable Catalogs

```bash
tvpilot search catalogs
```

### 2. Search Across Everything Searchable

```bash
tvpilot content search --query "top gun"
```

### 3. Search One Catalog

```bash
tvpilot content search --query "top gun" --catalog netflix
```

### 4. Search YouTube Catalog

```bash
tvpilot content search --query "lofi hip hop" --catalog youtube
```

### 5. Search Then Play First Result

```bash
tvpilot content search --query "top gun" --catalog netflix --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### 6. Search Across Everything Then Play

```bash
tvpilot content search --query "top gun" --limit 1 | \
tvpilot play --stdin --path .data.results[0].launch
```

### 7. Play From An Inline LaunchSpec

```bash
tvpilot play --launch-spec '{"app_handle":"youtube","app_display_name":"YouTube","app_id":"org.youtube","launch_uri":"youtube://watch?v=abc123","launch_extras":{},"launch_method":"deeplink"}'
```
