# tvpilot

Accessibility-first automation surface for LLM agents driving Tizen TV applications. See `PLAN.md` for the design and `SCHEMA.md` for the wire contract.

## Repository layout

- `PLAN.md`, `SCHEMA.md` — the actual project (design + wire schema). The implementation will live here.
- `aurum/`, `agent-browser/` — **reference only, not part of tvpilot.** These are external codebases we are studying and distilling from to build our own thing. Do not edit them. Do not treat their files as project source. Read them when we need to crib a pattern (e.g. agent-browser's `eN` ref design, Aurum's AT-SPI usage), then write fresh tvpilot code.

When tvpilot source lands, it will be in a sibling directory (e.g. `crates/` or `src/`), separate from the reference folders.

## Running on target

**Always go through `scripts/`. Never invoke `tvpilot` or `tizen-aurum-cli` directly via `rsdb shell` or `rsdb agent exec`.**

- `scripts/setup.sh` — one-time per TV boot. Launches `org.tizen.aurum-bootstrap` as owner so the a11y bus is enabled. Idempotent.
- `scripts/compare.sh` — runs `tvpilot snap` and `tizen-aurum-cli dump-tree` strictly as user `owner` (uid 5001) with `XDG_RUNTIME_DIR=/run/user/5001` set, and sends `tvpilot close` at the end so we don't leak the daemon.

The launching rule the scripts enforce:
1. `aurum-bootstrap` must be running first.
2. all clients run as owner (AT-SPI rejects other uids).
3. `tvpilot close` after every test session.

If you need a one-off snap or a different timing setup, extend `compare.sh` (or add a sibling script) — do not reach for `rsdb shell -- 'su - owner -c "..."'` on the fly. Past sessions accumulated stuck D-state processes from ad-hoc calls during AT-SPI syscalls, and those can only be cleared by a TV reboot.

Out of scope of this rule (these are fine to run directly): `rsdb push` for deploying binaries, `kill` to clean up stuck processes, `cargo tizen build` for local builds, and inspecting `/run/user/5001/tvpilot-raw.txt` or `tvpilot-rendered.txt` via `rsdb shell`.
