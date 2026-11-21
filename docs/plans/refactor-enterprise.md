# Refactor plan: modular, config-driven Harness

Status: phases 1–7 done (2026-11-21), see "Result" at the end. Phase 0 skipped.

## Goals

1. No file above ~400 lines (target 300, per `docs/architecture.md`); one file = one responsibility.
2. No behaviour change per step: every step builds, tests pass, app behaves the same.
3. Hardcoded values (agent lists, timeouts, limits, labels, colours) move to typed config / manifests.
4. Faster builds: UI and app logic in separate crates; daily test builds use `--profile quick`.
5. Clear extension points (traits) so later plugin/WASM work plugs in without touching core.

Non-goals now: plugin host, WASM, self-build, new features. Feature work pauses during the refactor.

## Current hotspots (lines, 2026-11-21)

| Lines | File | Problem |
| ---: | --- | --- |
| 5585 | `apps/harness-slint/ui/app.slint` | root window + sidebar + chat list + rows + composer + art in one file |
| 2802 | `apps/harness-slint/src/core/hub.rs` | adapter: command dispatch, event mapping, specs/env, tail loading |
| 2425 | `crates/harness-agent-hub/src/inbox.rs` | model + storage + sync + tests |
| 2411 | `apps/harness-slint/src/settings/mod.rs` | every settings page's state and handlers |
| 2343 | `crates/harness-agent-hub/src/session.rs` | actor loop, turn start, failover, queue, background |
| 2147 | `apps/harness-slint/ui/settings/config.slint` | many pages in one file |
| 1896 | `apps/harness-slint/ui/settings/agents.slint` | same |
| 1852 | `apps/harness-slint/src/bridge/turns.rs` | transcript model building |
| 1618 | `apps/harness-slint/src/chat/mod.rs` | palette, find, chips, send |
| 1583 | `crates/harness-agent-hub/src/hub.rs` | `HarnessCore` impl for every area |
| 1574 | `apps/harness-slint/src/bridge.rs` | UiState + model sync |
| 1339 | `apps/harness-slint/src/core/fake.rs` | fake core |
| 1334 | `apps/harness-slint/ui/chat/turns.slint` | all row kinds |

About 70 files exceed 600 lines; the table lists the ones that block daily work.

## Target layout

```
apps/harness-slint          thin exe: main.rs, helper modes, window chrome
crates/harness-ui           .slint only (slint-build), split per area; live-preview in dev
crates/harness-app          app logic: bridge/, chat/, settings/, panels/, core adapter
crates/harness-config       typed defaults + manifests loaded at runtime (agents, limits, labels)
crates/* (existing)         hub, api-agent, settings, remote, workspace, ...
```

Module rule for each area: `mod.rs` (wiring, <150 lines) + `state.rs` + `handlers.rs` + `model.rs` + `tests.rs`.

## Hardcode → config

| Today | Moves to |
| --- | --- |
| built-in agent specs (`agents.rs:79`), adapter packages, CLI packages | `harness-config/agents.toml` (embedded default, user override in `~/.harness/agents.toml`) |
| timeouts / limits (pump frame, tail turns/items, code-mode wall/memory/calls, search deadline) | `harness-config` `Limits` struct, overridable in `settings.json` |
| prompt fragments (STYLE, research, factcheck, code mode) | `harness-config/prompts/*.md`, embedded with `include_str!`, user override |
| theme colours / sizes scattered in `.slint` | `ui/theme/*.slint` tokens only (no literals in components) |
| labels duplicated in Rust and Slint | one source (`labels.slint` / Rust consts generated or shared) |

## Extension points (traits only, default impl = today's code)

- `AgentSelector` (fallback order / failover choice) — hub `quota.rs` + `session.rs`.
- `ToolProvider` (API agent tools) — `harness-api-agent/src/tools.rs`.
- `TranscriptFormatter` (row building) — `bridge/turns.rs`.
- `PromptSource` (system prompt pieces) — `harness-api-agent/src/session.rs`, hub `context.rs`.

## Phases (each phase = its own commit(s), shippable)

0. **Build speed**: test builds via `--profile quick`; add `lld` linker; measure before/after.
1. **Split `.slint`**: `app.slint` → `ui/shell/`, `ui/sidebar/`, `ui/chat/list.slint`, `ui/chat/rows/*.slint`, `ui/composer/`, `ui/brand/`; settings pages one file each. Pure moves.
2. **Split app Rust**: `core/hub.rs` → `core/hub/{dispatch,events,specs,tail,sessions,...}.rs`; `settings/mod.rs` → one module per page; `bridge.rs`, `bridge/turns.rs`, `chat/mod.rs` per responsibility.
3. **Crate split**: move `.slint` + generated code into `crates/harness-ui`, logic into `crates/harness-app`; `apps/harness-slint` becomes the thin exe.
4. **Split hub crate files**: `session.rs`, `hub.rs`, `inbox.rs`, `store.rs` per responsibility (actor/turn/failover/queue; `HarnessCore` impl per area).
5. **Config extraction**: `harness-config` crate + move the hardcode table above.
6. **Extension traits**: introduce the four traits with today's code as default impl.
7. **Quality gates**: CI (fmt, clippy -D warnings, tests per crate, quick build) on push; size check (fail if a file > 400 lines).

## Execution

- Subagents in parallel per phase, each owning disjoint files (`claim_paths`); orchestrator reviews diffs and runs `cargo check` / tests of touched crates.
- Verification per phase: `cargo test` for touched crates, `cargo check -p harness-slint`, one quick build and a manual smoke run by the user. Release build only at the end of a phase group.
- Rough effort: phases 0–3 ≈ 1 week of subagent work, 4–7 ≈ 1 more week (estimate, not measured).

## Decisions (user, 2026-11-21)

1. Test builds stay `--release`; every phase is verified for identical behaviour (tests, e2e gates, smoke run).
2. Crate names are free; priority is ease of development.
3. Order chosen by the orchestrator: phase 0 (quick profile) skipped; started with phase 1 + 2 in parallel:
   A `ui/app.slint`, B settings `.slint` pages, C `src/core/hub.rs`, D `src/settings/mod.rs`.
   Source backup before the refactor: `~/Downloads/worse-pre-refactor-20261121.zip`.

## Result (2026-11-21)

- Phases 1, 2, 4: every Rust and Slint source file is at most 400 lines except the reasoned entries in
  `scripts/size-allow.json` (the `HarnessCore` trait and its single impl, two frozen legacy files, root
  window properties, generated ASCII art).
- Phase 3: `.slint` + translations live in `crates/harness-ui` (`slint-build`; translation file
  `lang/zh_CN/LC_MESSAGES/harness-ui.po`). The app logic stays in the `apps/harness-slint` package as the
  library `harness_slint` (`src/lib.rs`, `pub fn run`); `src/main.rs` is a 5-line exe. No separate
  `harness-app` crate: the name belongs to the legacy Tauri package, and one package keeps
  `CARGO_PKG_VERSION` single-sourced for releases.
- Phase 5: `crates/harness-config` — `assets/agents.toml` (built-in agents; user additions stay in
  `~/.harness/agents.json`), `Limits` (agent idle, shutdown grace, event capacity, schedule interval,
  subagent quota), `assets/prompts/api-agent-system.md` (user override
  `<HARNESS_HOME>/prompts/api-agent-system.md`), `harness_home()`. Open: `Limits` overrides from
  `settings.json`, the remaining prompt fragments (code mode, research, factcheck), theme literals, shared labels.
- Phase 6: `ToolProvider` (`harness-api-agent/src/tools/provider.rs`: web, factcheck), `PromptSource`
  (`harness-api-agent/src/prompt_source.rs`), `AgentSelector` (`harness-agent-hub/src/selector.rs`,
  `HubConfig::agent_selector`), `TranscriptFormatter` (`apps/harness-slint/src/chat/export/formats.rs`).
- Phase 7: `.github/workflows/ci.yml` (size, fmt, clippy -D warnings, product crate tests, quick build),
  `scripts/size-check.mjs`, also run by `scripts/check.ps1` / `check.sh`.
- Verified: fmt, workspace clippy, `cargo test --workspace`, gates phase8-core, phase9-chat,
  phase9-settings, release build. `phase9-api-agent` fails identically on the pre-refactor HEAD (its mock
  matches string message content only).
