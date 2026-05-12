# tvpilot

Accessibility-first automation surface for LLM agents driving Tizen TV applications. See `PLAN.md` for the design and `SCHEMA.md` for the wire contract.

## Repository layout

- `PLAN.md`, `SCHEMA.md` — the actual project (design + wire schema). The implementation will live here.
- `aurum/`, `agent-browser/` — **reference only, not part of tvpilot.** These are external codebases we are studying and distilling from to build our own thing. Do not edit them. Do not treat their files as project source. Read them when we need to crib a pattern (e.g. agent-browser's `eN` ref design, Aurum's AT-SPI usage), then write fresh tvpilot code.

When tvpilot source lands, it will be in a sibling directory (e.g. `crates/` or `src/`), separate from the reference folders.
