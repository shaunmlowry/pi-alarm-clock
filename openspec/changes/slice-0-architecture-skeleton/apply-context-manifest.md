# Slice 0 Apply Context Manifest

Purpose: keep each implementation session's context **minimal and scoped to one task group**, so a 65k-context local model (Qwen 3.6) can complete the slice without compaction. The state that survives between sessions is only what's on disk — the code and the checked-off `tasks.md`. Nothing else.

This is the single source of truth for the apply loop. A script (or a human driving manually) reads this manifest + `tasks.md` + the design/spec files and runs one session per group.

---

## How to use this manifest

For each group below, a session is:

1. **Build the prompt** from the group's `design`, `spec`, `task-ids`, `read-files`, and `verify` fields. The design/spec sections are **extracted by header anchor** from `design.md` / `specs/<cap>/spec.md` (see *Extraction contract* below) — not hand-quoted, so they never drift.
2. **Run** `pi -p` with the prompt piped via stdin, scoped flags (see *Invocation contract*).
3. **Verify**: after pi exits, run the group's `verify` command. Gate on its exit code AND on the checkbox-flip check (the box(es) for the group's `task-ids` must be `[x]` in `tasks.md`).
4. **Gate**: verify pass + boxes flipped → next group. Verify fail OR boxes still `[ ]` → **STOP, surface to human.** Never auto-advance on red.

The model is told to do exactly the group's tasks, flip the boxes, run the verify command itself, and stop. It is explicitly told **not** to implement other tasks.

---

## Extraction contract

The script extracts sections by header anchor so the injected context is current without hand-copying.

- **Design decisions** (`design.md`): decisions live under `## Decisions` as `### D<n>. <title>`. Extract from the line `### D<n>.` up to (but not including) the next line starting with `### D` or `## `. A group's `design` field lists decision IDs, e.g. `D2, D1`.
- **Spec requirements** (`specs/<capability>/spec.md`): each requirement is `### Requirement: <name>` followed by `#### Scenario: ...` blocks. Extract from `### Requirement: <name>` up to the next `### Requirement:` or EOF. A group's `spec` field is `<capability> > <requirement name>` (may repeat).
- **Tasks** (`tasks.md`): the group's `task-ids` (e.g. `2.1, 2.2`) are looked up; their full text lines (`- [ ] 2.1 ...`) are injected so the model sees the exact wording.

`read-files` are **not** inlined by the script — the prompt instructs pi to `read` them at session start (keeps the manifest small and the file contents current). For greenfield early groups, `read-files` is empty.

---

## Invocation contract (script)

```bash
pi -p --no-skills --no-context-files -a --name "slice0-<group-id>" <<EOF
<prompt built from this manifest>
EOF
```

- `-p` — non-interactive, runs tools, prints, exits.
- `--no-skills` — do NOT auto-load the `openspec-apply-change` skill (it would instruct loading all context files — the anti-pattern we're avoiding).
- `--no-context-files` — no `AGENTS.md`/`CLAUDE.md` auto-discovery (none exist yet, but defensive).
- `-a` — trust project-local resources for this run (so `.pi/settings.json` loads). Or set `defaultProjectTrust: "always"` once in `~/.pi/agent/settings.json` and drop the flag.
- `--name` — saved session for auditability (optional: `--no-session` for ephemeral).
- Optional: add `--mode json` and tee to a per-group `.log` if you want structured event capture for debugging. For gating, `-p` plain + the cargo verify is sufficient.

**Prompt scaffold** (built per group):

```
You are implementing ONE task group in a Rust alarm-clock project (slice 0 of an OpenSpec change). Work in the current directory.

PROJECT ROOT: /home/shaun/pi-alarm-clock

TASKS (do exactly these, nothing else):
<injected task lines from tasks.md for this group's task-ids>

DESIGN CONTEXT (read carefully — this is the architecture you must follow):
<extracted design decision(s)>

SPEC (the requirement(s) these tasks satisfy; scenarios are your acceptance tests):
<extracted requirement(s) + scenarios>

EXISTING CODE (read these files before writing; do not modify unrelated files):
<read-files list, or "none — greenfield" if empty>

INSTRUCTIONS:
- Make minimal, focused changes for ONLY the tasks above.
- Do not implement future tasks or speculate about later slices.
- After completing each task, mark its checkbox in tasks.md: change "- [ ]" to "- [x]" for that task id.
- Use the bash, read, edit, write tools as needed.
- Before finishing, run the VERIFY command and ensure it passes. If it fails, fix and re-run.

VERIFY (run this before finishing; it must pass):
<verify command>

When done: confirm the verify command passed and the task checkboxes are flipped, then stop.
```

---

## Groups

Each group is one session. `task-ids` maps to `tasks.md`. `≈tokens` is a rough estimate of injected context (design+spec+prompt), excluding file reads — well under the 65k window for every group.

### Group 1 — Workspace & dependencies
- **task-ids**: `1.1, 1.2`
- **design**: `D1` (threading model — sets why workspace is split this way)
- **spec**: none (infrastructure)
- **read-files**: none (greenfield)
- **write**: `Cargo.toml` (workspace), `alarm-clock/Cargo.toml`, `alarm-clock/src/main.rs` (empty `fn main`), `mopidy-client/Cargo.toml`, `mopidy-client/src/lib.rs`, `rust-toolchain.toml`
- **verify**: `cargo check --workspace` exits 0
- **≈tokens**: ~1.5k

### Group 2 — Bootstrap config
- **task-ids**: `1.3, 1.4`
- **design**: `D8` (bootstrap config)
- **spec**: `process-runtime > Bootstrap configuration via TOML file`
- **read-files**: `Cargo.toml`, `alarm-clock/Cargo.toml`
- **write**: `alarm-clock/src/config.rs`, `config.toml`
- **verify**: `cargo check -p alarm-clock` + a `#[test]` that parses a partial toml and asserts defaults fill missing fields
- **≈tokens**: ~2k

### Group 3 — Channels & message enums
- **task-ids**: `2.1`
- **design**: `D2` (channel topology)
- **spec**: `process-runtime > Bounded event channel with drop-oldest`, `process-runtime > Non-blocking reply consumption on main`
- **read-files**: `alarm-clock/src/main.rs`, `mopidy-client/src/lib.rs`
- **write**: `alarm-clock/src/chan.rs` (or `ipc.rs`) — `Cmd`, `Reply`, `MopidyEvent` enums + channel constructors
- **verify**: `cargo check -p alarm-clock` + a `#[test]` that floods the event channel past capacity and asserts drop + that `warn!` would fire (assert channel len == cap after flood)
- **≈tokens**: ~2.5k

### Group 4 — Tokio worker + drain timer
- **task-ids**: `2.2`
- **design**: `D1`, `D2`
- **spec**: `process-runtime > Single Rust process with two-thread architecture`, `process-runtime > Non-blocking reply consumption on main`
- **read-files**: `alarm-clock/src/main.rs`, `alarm-clock/src/chan.rs`
- **write**: `alarm-clock/src/runtime.rs` — tokio worker thread spawn + `slint::Timer` drain that `try_recv`s replies/events
- **verify**: `cargo check -p alarm-clock` + a `#[test]`/example that spawns the worker, sends a noop `Cmd`, and asserts the reply is drained on a tick
- **≈tokens**: ~3k
- **note**: Slint timer API specifics (`slint::Timer::start`/`default` + `invoke_from_event_loop`) — if the model stalls on API, allow it to read `docs` of slint; otherwise keep scoped.

### Group 5 — Observability (tracing → journald)
- **task-ids**: `2.3`
- **design**: `D5` (observability)
- **spec**: `process-runtime > Structured logging to journald`
- **read-files**: `alarm-clock/src/main.rs`
- **write**: `alarm-clock/src/log.rs` — subscriber init (journald layer + fmt fallback), span names registered
- **verify**: `cargo check -p alarm-clock` + a `#[test]`/example that initializes the subscriber and emits a spanned `info!` line (capture via `tracing_subscriber::fmt().with_test_writer()`)
- **≈tokens**: ~1.5k

### Group 6 — Panic isolation & error policy
- **task-ids**: `2.4, 2.5`
- **design**: `D6` (error & panic policy)
- **spec**: `process-runtime > Tick-level panic isolation`, `process-runtime > Failed config writes degrade, not panic`
- **read-files**: `alarm-clock/src/main.rs`, `alarm-clock/src/runtime.rs`
- **write**: `alarm-clock/src/tick.rs` (catch_unwind wrapper) + domain error types (`thiserror`) in `alarm-clock/src/error.rs`
- **verify**: `cargo check -p alarm-clock` + a `#[test]` that a panicking tick body is caught and the wrapper returns (not panics)
- **≈tokens**: ~2k

### Group 7 — Shutdown seam & systemd readiness
- **task-ids**: `2.6, 2.7`
- **design**: `D7` (graceful shutdown seam), `D10` (systemd readiness)
- **spec**: `process-runtime > Graceful shutdown seam`, `process-runtime > systemd Type=notify readiness`
- **read-files**: `alarm-clock/src/main.rs`, `alarm-clock/src/runtime.rs`
- **write**: `alarm-clock/src/shutdown.rs` — SIGTERM/SIGINT listener on tokio, shutdown coord, `shutdown_restore()` no-op hook; `sd_notify` call gated behind a `systemd` feature or runtime detection
- **verify**: `cargo check -p alarm-clock` + a `#[test]` that `shutdown_restore()` no-op is callable
- **≈tokens**: ~3k
- **note**: `sd_notify` — use the `sd-notify` crate (or inline `libc::sendto` to `SOCK_DGRAM` to the notify socket). Guard with env-var presence (`NOTIFY_SOCKET`) so non-systemd dev boots fine.

### Group 8 — SQLite open & migration runner
- **task-ids**: `3.1, 3.2`
- **design**: `D3` (SQLite, migrations)
- **spec**: `persistence > SQLite store with WAL mode`, `persistence > Versioned migrations on startup`
- **read-files**: `alarm-clock/src/main.rs`
- **write**: `alarm-clock/src/db.rs` — `Connection` open with WAL pragmas + `user_version`-based migration runner trait
- **verify**: `cargo check -p alarm-clock` + a `#[test]` opening `:memory:` asserts `journal_mode=wal`† and `user_version=0` initially
- **≈tokens**: ~2k
- **note**: †WAL on `:memory:` is not persisted/supported the same way — the test should assert pragmas are *set* (return `wal`/`normal`) on a temp-file db, and use `:memory:` only for migration-runner tests. Flag this nuance to the model.

### Group 9 — Migration v1 & ConfigStore
- **task-ids**: `3.3, 3.4, 3.5`
- **design**: `D3`
- **spec**: `persistence > Versioned migrations on startup` (v1 content), `persistence > ConfigStore abstraction on main`, `persistence > Atomic config mutations`
- **read-files**: `alarm-clock/src/db.rs`
- **write**: migration `v1` SQL in `alarm-clock/src/db.rs` (or `migrations/`); `alarm-clock/src/config_store.rs` — `get`/`set` over `kv_config`, single-transaction mutations
- **verify**: `cargo test -p alarm-clock` — (a) fresh `:memory:` migrates to v1, `schema_meta`+`kv_config` exist, `user_version=1`; (b) `set("last_boot", X)` then `get("last_boot")` returns X; (c) partial-failure transaction rolls back (craft a two-statement mutation where the second errors, assert neither row present)
- **≈tokens**: ~3k

### Group 10 — Mopidy WS transport & reconnect
- **task-ids**: `4.1, 4.2`
- **design**: `D4` (Mopidy client)
- **spec**: `mopidy-client > Reconnecting WebSocket JSON-RPC client`, `mopidy-client > Indefinite reconnect with bounded backoff`
- **read-files**: `mopidy-client/src/lib.rs`
- **write**: `mopidy-client/src/transport.rs` (WS connect + JSON-RPC framing) + `mopidy-client/src/reconnect.rs` (backoff state machine)
- **verify**: `cargo check -p mopidy-client` + a `#[test]` for backoff params (assert next delay ≤ cap, jitter within ±20%)
- **≈tokens**: ~3k
- **note**: live Mopidy not required for unit tests; mock the WS or test backoff math in isolation.

### Group 11 — Connection-state signal
- **task-ids**: `4.3`
- **design**: `D4`
- **spec**: `mopidy-client > Connection-state signal`
- **read-files**: `mopidy-client/src/transport.rs`, `mopidy-client/src/reconnect.rs`, `alarm-clock/src/chan.rs`
- **write**: `MopidyConnectionState` enum + publish-on-transition via the reply channel
- **verify**: `cargo check -p mopidy-client` + a `#[test]` that a transition produces a `Reply` of the right variant
- **≈tokens**: ~2k

### Group 12 — Typed methods
- **task-ids**: `4.4`
- **design**: `D4`
- **spec**: `mopidy-client > Typed minimal method surface`
- **read-files**: `mopidy-client/src/transport.rs`
- **write**: `mopidy-client/src/methods.rs` — `core.get_version`, `core.get_state` typed request/reply structs + `call`
- **verify**: `cargo check -p mopidy-client` + a `#[test]` that a request serializes to the expected JSON-RPC envelope and a sample reply deserializes into the typed struct
- **≈tokens**: ~2k

### Group 13 — Event parsing
- **task-ids**: `4.5`
- **design**: `D4`
- **spec**: `mopidy-client > Event channel`
- **read-files**: `mopidy-client/src/transport.rs`, `alarm-clock/src/chan.rs`
- **write**: `mopidy-client/src/events.rs` — parse incoming `method`-bearing messages into `MopidyEvent`, forward to event channel
- **verify**: `cargo check -p mopidy-client` + a `#[test]` parsing a sample `playback_state_changed` payload into the enum
- **≈tokens**: ~2k

### Group 14 — Mopidy end-to-end (manual / on Pi)
- **task-ids**: `4.6`
- **design**: `D4`
- **spec**: all `mopidy-client` scenarios
- **read-files**: whole `mopidy-client/src/`
- **write**: possibly a small `examples/probe.rs` to drive a round-trip
- **verify**: against a running Mopidy — `core.get_version` returns a typed reply; `core.get_state` returns a state; stop Mopidy → backoff transitions → restart Mopidy → reconnects to `Connected`
- **≈tokens**: ~2.5k
- **note**: this is a **manual acceptance session**, not a CI gate. Run interactively. Skip if Mopidy isn't running; come back at integration time.

### Group 15 — Slint app & nav scaffold
- **task-ids**: `5.1, 5.2`
- **design**: `D9` (UI shell)
- **spec**: `ui-shell > Slint application with vertical orientation`, `ui-shell > Multi-panel navigation scaffold`
- **read-files**: `alarm-clock/src/main.rs`, `alarm-clock/src/runtime.rs`
- **write**: `alarm-clock/ui/main.slin` (or inline `slint!`), panel container with hard-stop swipe, one panel slot
- **verify**: `cargo check -p alarm-clock` (Slint build macro compiles) + `cargo run` shows a window (manual eyeball on dev machine is fine; full Pi check at Group 18)
- **≈tokens**: ~2.5k

### Group 16 — Clock panel & theme seam
- **task-ids**: `5.3, 5.4`
- **design**: `D9`
- **spec**: `ui-shell > Clock panel with reserved theme seam`
- **read-files**: `alarm-clock/ui/main.slin`
- **write**: Clock panel component with exposed `clock_color`, `font_family` properties fed by hardcoded values; ensure vertical gestures not consumed by nav
- **verify**: `cargo check -p alarm-clock` + `cargo run` shows the placeholder clock (manual)
- **≈tokens**: ~1.5k

### Group 17 — systemd unit
- **task-ids**: `6.1`
- **design**: `D10`
- **spec**: `process-runtime > systemd Type=notify readiness`
- **read-files**: `alarm-clock/src/main.rs`, `alarm-clock/src/shutdown.rs`
- **write**: `dist/alarm-clock.service` (Type=notify, Restart=on-failure + StartLimit*, After/Wants network-online + mopidy, no blocking)
- **verify**: `systemd-analyze verify dist/alarm-clock.service` exits 0
- **≈tokens**: ~2k

### Group 18 — On-Pi acceptance (manual)
- **task-ids**: `6.2, 6.3, 6.4, 6.5, 6.6`
- **design**: `D10`, `D5`
- **spec**: all remaining `process-runtime` + `ui-shell` scenarios
- **read-files**: as needed
- **write**: none (verification only; fix-forward any defects found, re-running the relevant earlier group's session)
- **verify** (manual checklist on the Pi):
  - `systemctl start` → `active (running)`, `READY=1` even with Mopidy down
  - screen shows placeholder Clock, vertical orientation
  - `journalctl` shows structured entries + Mopidy state transitions/events
  - reboot → migration idempotent (`user_version=1`), `ConfigStore` round-trips, `READY=1`
  - `systemctl stop` → exit 0, clean shutdown log sequence
- **≈tokens**: ~3k
- **note**: this is a human-driven acceptance pass. If a defect surfaces, re-run the relevant earlier group's session with the defect as an additional instruction.

---

## Group dependency order

Linear — each group depends on the prior's on-disk output:

```
1 → 2 → 3 → 4 → 5 → 6 → 7
                ↘ 8 → 9 ↗
4 → 10 → 11 → 12 → 13 → 14(manual)
4 → 15 → 16
7 → 17 → 18(manual)
```

`5` and `6` are independent of each other but both after `4`. `8`/`9` (persistence) branch off `4` and merge before `7`'s shutdown needs the store? No — `7` (shutdown seam) doesn't need the store in slice 0 (no-op restore). So `8`/`9` can run any time after `4`. The merge before `18` (acceptance) is the only hard cross-branch requirement: everything done before acceptance.

**Suggested run order**: 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17, then 14 & 18 manually. (14 is a manual Mopidy check; can defer to integration with 18.)

---

## Watch-outs (things to verify in the first 2–3 groups before unattended runs)

1. **Does Qwen follow "stop after this group, don't implement future tasks"?** Small models love to over-implement. If it does, the prompt needs a harder constraint (or a post-run check that `git diff` only touches expected files). Add a `git diff --stat` review to the gate for the first few groups.
2. **Does Qwen flip the checkbox reliably?** If it forgets, the script's checkbox-flip check catches it and stops — but if it happens often, add "MANDATORY: flip the checkbox" as the last line of the prompt.
3. **Does `cargo check` catch the model's mistakes well enough, or do we need `cargo clippy -- -D warnings`?** Clippy is stricter; consider it as the verify for code-touching groups once `cargo check` proves too loose.
4. **Slint API specifics** (Group 4, 15, 16): if Qwen hallucinates `slint::Timer` or component syntax, allow it to read the slint docs / its own `Cargo.lock`-pinned source. Budget for one or two re-runs here.

---

## What this manifest deliberately does NOT include

- **The PRD.** Not needed for any slice-0 task; the specs already captured the relevant decisions. Re-feeding 22.8KB to every session is waste.
- **The openspec-apply skill.** Explicitly suppressed via `--no-skills` — it would instruct loading all context files, the anti-pattern we're avoiding.
- **Cross-task memory.** None. Each session is fresh; the on-disk code is the only memory. This is the feature, not a limitation.
- **Compaction tolerance.** None assumed. If any single group blows the window (none should in slice 0), that group is too big — split it rather than lean on compaction.
