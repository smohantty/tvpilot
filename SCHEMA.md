# tvpilot Core Schema

This file defines the core composable schema for:

- `app list`
- `app launch`
- `content search`
- supporting content-source discovery needed to make search predictable

It focuses on the shared envelope, reusable types, command input/output, and how those commands compose cleanly for an autonomous agent.

---

## Shared Envelope

Every one-shot command returns the same top-level envelope:

```jsonc
{
  "ok": true,
  "cmd": "app.list",
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
  "cmd": "app.launch",
  "rid": "req-42",
  "data": null,
  "error": {
    "code": "APP_NOT_FOUND",
    "msg": "No installed app matches handle 'netflix'",
    "retryable": false,
    "hint": "app list"
  },
  "ts": "2026-03-18T08:21:00Z",
  "ms": 7,
  "v": "0.1.0"
}
```

---

## Shared Types

### AppIdentity

Used anywhere the CLI identifies an app/provider.

```jsonc
{
  "handle": "netflix",
  "display_name": "Netflix",
  "app_id": "org.netflix",
  "version": "9.12.0",
  "type": "web"
}
```

Field meanings:

- `handle`: canonical agent-facing slug
- `display_name`: human-facing label
- `app_id`: exact platform-native identifier
- `version`: installed app version when known
- `type`: native/web/system classification when known

### LaunchSpec

Normalized launch object produced by content APIs and consumable by `app launch --stdin`.

```jsonc
{
  "app": "netflix",
  "display_name": "Netflix",
  "app_id": "org.netflix",
  "uri": "netflix://watch/80057281",
  "extras": {},
  "backend": "deeplink"
}
```

Field meanings:

- `app`: canonical handle accepted by `app launch <app_ref>`
- `display_name`: app label for logs and debugging
- `app_id`: exact resolved launcher target
- `uri`: deep-link or launch URI
- `extras`: app-control extras map
- `backend`: launch mechanism, for example `deeplink`, `app_control`, `native`

### SearchSource

Describes where search is executed.

```jsonc
{
  "id": "searchon",
  "display_name": "Samsung Search",
  "kind": "aggregator",
  "available": true,
  "providers": ["netflix", "disney", "tv_plus"],
  "adapter": "searchon_catalog"
}
```

Field meanings:

- `id`: canonical search source id
- `display_name`: human-facing source name
- `kind`: `aggregator`, `provider_native`, or other backend class
- `available`: whether the source can currently be queried
- `providers`: provider handles this source may return
- `adapter`: internal adapter used to talk to the source

### ContentSearchResult

```jsonc
{
  "content_id": "netflix:series:80057281",
  "dedupe_key": "imdb:tt4574334",
  "title": "Stranger Things",
  "type": "series",
  "source": "searchon",
  "source_display_name": "Samsung Search",
  "provider": "netflix",
  "provider_display_name": "Netflix",
  "year": 2016,
  "rating": "TV-14",
  "synopsis": "A group of kids uncover a supernatural mystery.",
  "launchable": true,
  "auth_required": false,
  "launch": {
    "app": "netflix",
    "display_name": "Netflix",
    "app_id": "org.netflix",
    "uri": "netflix://watch/80057281",
    "extras": {},
    "backend": "deeplink"
  },
  "adapter": "provider_catalog",
  "confidence": 0.97
}
```

### Source vs Provider

- `source` answers: where did `tvpilot` find this result?
- `provider` answers: where can this result actually be launched or played?

Examples:

- `source=searchon`, `provider=netflix`
- `source=youtube`, `provider=youtube`

This distinction lets `tvpilot` federate search across aggregators and provider-native search without losing launch precision.

---

## Command Schemas

### 1. `app list`

CLI:

```bash
tvpilot app list
tvpilot app list --running
```

Conceptual input:

```jsonc
{
  "cmd": "app.list",
  "args": {
    "running": false
  }
}
```

`data` shape:

```jsonc
{
  "apps": [
    {
      "handle": "netflix",
      "display_name": "Netflix",
      "app_id": "org.netflix",
      "version": "9.12.0",
      "type": "web"
    }
  ]
}
```

`app list --running` adds runtime fields per entry:

```jsonc
{
  "apps": [
    {
      "handle": "youtube",
      "display_name": "YouTube",
      "app_id": "org.youtube",
      "pid": 4182,
      "state": "foreground"
    }
  ]
}
```

Full example:

```jsonc
{
  "ok": true,
  "cmd": "app.list",
  "rid": null,
  "data": {
    "apps": [
      {
        "handle": "netflix",
        "display_name": "Netflix",
        "app_id": "org.netflix",
        "version": "9.12.0",
        "type": "web"
      },
      {
        "handle": "youtube",
        "display_name": "YouTube",
        "app_id": "org.youtube",
        "version": "4.33.1",
        "type": "web"
      }
    ]
  },
  "error": null,
  "ts": "2026-03-18T08:21:00Z",
  "ms": 14,
  "v": "0.1.0"
}
```

### 2. `app launch`

CLI:

```bash
tvpilot app launch netflix
tvpilot app launch netflix --uri "netflix://watch/80057281"
tvpilot app launch --stdin
tvpilot app launch --stdin --path .data.results[0].launch
```

Accepted inputs:

Direct app reference:

```jsonc
{
  "cmd": "app.launch",
  "args": {
    "app_ref": "netflix",
    "uri": "netflix://watch/80057281",
    "extras": {}
  }
}
```

Launch object from stdin:

```jsonc
{
  "app": "netflix",
  "display_name": "Netflix",
  "app_id": "org.netflix",
  "uri": "netflix://watch/80057281",
  "extras": {},
  "backend": "deeplink"
}
```

`data` shape:

```jsonc
{
  "handle": "netflix",
  "display_name": "Netflix",
  "app_id": "org.netflix",
  "pid": 4182,
  "uri": "netflix://watch/80057281",
  "backend": "deeplink"
}
```

Full example:

```jsonc
{
  "ok": true,
  "cmd": "app.launch",
  "rid": "req-launch-1",
  "data": {
    "handle": "netflix",
    "display_name": "Netflix",
    "app_id": "org.netflix",
    "pid": 4182,
    "uri": "netflix://watch/80057281",
    "backend": "deeplink"
  },
  "error": null,
  "ts": "2026-03-18T08:21:03Z",
  "ms": 31,
  "v": "0.1.0"
}
```

### 3. `content search`

CLI:

```bash
tvpilot content search --query "stranger things"
tvpilot content search --query "stranger things" --provider netflix --limit 1
tvpilot content search --query "stranger things" --source searchon --provider netflix --limit 1
tvpilot content search --query "stranger things" --scope installed_searchable
```

Conceptual input:

```jsonc
{
  "cmd": "content.search",
  "args": {
    "query": "stranger things",
    "sources": ["searchon"],
    "providers": ["netflix"],
    "scope": "installed_searchable",
    "type": null,
    "limit": 1,
    "cursor": null
  }
}
```

`data` shape:

```jsonc
{
  "query": "stranger things",
  "scope": "installed_searchable",
  "sources": ["searchon"],
  "results": [
    {
      "content_id": "netflix:series:80057281",
      "dedupe_key": "imdb:tt4574334",
      "title": "Stranger Things",
      "type": "series",
      "source": "searchon",
      "source_display_name": "Samsung Search",
      "provider": "netflix",
      "provider_display_name": "Netflix",
      "year": 2016,
      "rating": "TV-14",
      "synopsis": "A group of kids uncover a supernatural mystery.",
      "launchable": true,
      "auth_required": false,
      "launch": {
        "app": "netflix",
        "display_name": "Netflix",
        "app_id": "org.netflix",
        "uri": "netflix://watch/80057281",
        "extras": {},
        "backend": "deeplink"
      },
      "adapter": "provider_catalog",
      "confidence": 0.97
    }
  ]
}
```

Full example:

```jsonc
{
  "ok": true,
  "cmd": "content.search",
  "rid": null,
  "data": {
    "query": "stranger things",
    "scope": "installed_searchable",
    "sources": ["searchon"],
    "results": [
      {
        "content_id": "netflix:series:80057281",
        "dedupe_key": "imdb:tt4574334",
        "title": "Stranger Things",
        "type": "series",
        "source": "searchon",
        "source_display_name": "Samsung Search",
        "provider": "netflix",
        "provider_display_name": "Netflix",
        "year": 2016,
        "rating": "TV-14",
        "synopsis": "A group of kids uncover a supernatural mystery.",
        "launchable": true,
        "auth_required": false,
        "launch": {
          "app": "netflix",
          "display_name": "Netflix",
          "app_id": "org.netflix",
          "uri": "netflix://watch/80057281",
          "extras": {},
          "backend": "deeplink"
        },
        "adapter": "provider_catalog",
        "confidence": 0.97
      }
    ]
  },
  "error": null,
  "ts": "2026-03-18T08:21:02Z",
  "ms": 25,
  "v": "0.1.0"
}
```

Search semantics:

- `sources` chooses where the query is executed
- `providers` filters where returned content must be launchable
- `scope` chooses the default search-space preset when `sources` is not explicitly supplied

Recommended default scope:

```jsonc
{
  "scope": "installed_searchable"
}
```

Useful scopes:

- `installed_searchable`
- `all_available`
- `provider_native_only`
- `aggregators_only`

### 4. `content sources`

This is the supporting discovery command for the search API.

CLI:

```bash
tvpilot content sources
```

`data` shape:

```jsonc
{
  "sources": [
    {
      "id": "searchon",
      "display_name": "Samsung Search",
      "kind": "aggregator",
      "available": true,
      "providers": ["netflix", "disney", "tv_plus"],
      "adapter": "searchon_catalog"
    },
    {
      "id": "youtube",
      "display_name": "YouTube",
      "kind": "provider_native",
      "available": true,
      "providers": ["youtube"],
      "adapter": "youtube_native_search"
    }
  ]
}
```

---

## Composition Examples

### Search -> Launch Directly

The cleanest composition is to pipe the launch object straight into `app launch`.

```bash
tvpilot content search --query "dark knight" --scope installed_searchable --limit 1 | \
tvpilot app launch --stdin --path .data.results[0].launch
```

### Search -> Pick -> Launch

Useful when the agent wants to inspect or override pieces before launching.

```bash
result=$(tvpilot content search --query "stranger things" --source searchon --provider netflix --limit 1)
app=$(echo "$result" | tvpilot pick .data.results[0].launch.app --stdin --raw)
uri=$(echo "$result" | tvpilot pick .data.results[0].launch.uri --stdin --raw)
tvpilot app launch "$app" --uri "$uri"
```

### Search Source Discovery -> Search -> Launch

Useful when the agent wants to see which sources exist before deciding how broad the search should be.

```bash
tvpilot content sources
tvpilot content search --query "stranger things" --scope installed_searchable --limit 5
tvpilot content search --query "stranger things" --source searchon --provider netflix --limit 1 | \
tvpilot app launch --stdin --path .data.results[0].launch
```

### List -> Choose -> Launch

Useful when the agent wants to confirm install state or inspect the exact resolved app identity first.

```bash
tvpilot app list
tvpilot app launch netflix
```

### End-to-End Flow

The agent can use `content search` for discovery, inspect the first result's `launch` object, and then pass that object into `app launch` without inventing a second schema.

```bash
search=$(tvpilot content search --query "stranger things" --scope installed_searchable --limit 1)
echo "$search" | tvpilot pick .data.results[0].launch --stdin
echo "$search" | tvpilot app launch --stdin --path .data.results[0].launch
```

That shared `LaunchSpec` is the key composition contract between federated content discovery and app execution.
