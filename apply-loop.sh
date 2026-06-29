#!/usr/bin/env bash
#
# apply-loop.sh — drive slice-0 implementation group-by-group through pi.
#
# For each group runs a mini ralph-loop (max MAX_ITER iterations):
#   Iteration 1:
#     1. Build a scoped implementer prompt (design + spec extracted by header
#        anchor, task lines from tasks.md) and pipe to `pi -p` in a FRESH session.
#     2. Build a validator prompt and pipe to a second FRESH `pi -p` session.
#   Iteration N (on FAIL):
#     3. Build a repair prompt that includes the full validator output from
#        iteration N-1 as feedback. Pipe to a new FRESH `pi -p` session.
#     4. Re-validate.
#   On PASS, commit so the next group sees a clean baseline.
#
# Freshness guarantee: no -c/-r/--session flags are passed, so every `pi -p`
# invocation starts with an empty context window. --name saves each session for
# audit but does NOT load it into the next run.
#
# Usage:
#   ./apply-loop.sh                # run from first pending group
#   ./apply-loop.sh 4              # run only group 4
#   ./apply-loop.sh 4 7            # run groups 4 then 7
#
# Resume: just re-run; already-done groups (all task boxes [x]) are skipped.

set -euo pipefail

ROOT="/home/shaun/pi-alarm-clock"
CHANGE="$ROOT/openspec/changes/slice-0-architecture-skeleton"
DESIGN="$CHANGE/design.md"
SPECS="$CHANGE/specs"
TASKS="$CHANGE/tasks.md"
APPLYDIR="$CHANGE/.apply"
PROMPTDIR="$APPLYDIR/prompts"
LOGDIR="$APPLYDIR/logs"
PI_MODEL=${PI_MODEL:-"z-ai/glm-5.2"}
MAX_ITER=5
mkdir -p "$PROMPTDIR" "$LOGDIR"

# pi invocation: print mode, scoped (no skills, no context files), trust project.
PI=(pi -p --no-skills --no-context-files -a --model $PI_MODEL)

cd "$ROOT"

# ─── Group table ───────────────────────────────────────────────────────────
# Fields are `|`-separated: num|name|taskids|design_ids|spec_refs|files|verify
#   taskids, design_ids : comma-separated
#   spec_refs           : `;`-separated `cap>Requirement name`
#   files               : `;`-separated repo-relative paths (empty = greenfield)
#   verify              : shell command, or `MANUAL` (skipped, not failed)
TASK_GROUPS=(
"01|workspace-deps|1.1,1.2|D1|||cargo check --workspace"
"02|bootstrap-config|1.3,1.4|D8|process-runtime>Bootstrap configuration via TOML file|alarm-clock/Cargo.toml;alarm-clock/Cargo.toml|cargo test -p alarm-clock"
"03|channels|2.1|D2|process-runtime>Bounded event channel with drop-oldest;process-runtime>Non-blocking reply consumption on main||cargo test -p alarm-clock"
"04|tokio-worker|2.2|D1,D2|process-runtime>Single Rust process with two-thread architecture;process-runtime>Non-blocking reply consumption on main|alarm-clock/src/main.rs;alarm-clock/src/chan.rs|cargo test -p alarm-clock"
"05|observability|2.3|D5|process-runtime>Structured logging to journald|alarm-clock/src/main.rs|cargo test -p alarm-clock"
"06|panic-policy|2.4,2.5|D6|process-runtime>Tick-level panic isolation;process-runtime>Failed config writes degrade, not panic|alarm-clock/src/main.rs;alarm-clock/src/runtime.rs|cargo test -p alarm-clock"
"07|shutdown-sdnotify|2.6,2.7|D7,D10|process-runtime>Graceful shutdown seam;process-runtime>systemd Type=notify readiness|alarm-clock/src/main.rs;alarm-clock/src/runtime.rs|cargo test -p alarm-clock"
"08|sqlite-migrations|3.1,3.2|D3|persistence>SQLite store with WAL mode;persistence>Versioned migrations on startup|alarm-clock/src/main.rs|cargo test -p alarm-clock"
"09|migration-v1-configstore|3.3,3.4,3.5|D3|persistence>Versioned migrations on startup;persistence>ConfigStore abstraction on main;persistence>Atomic config mutations|alarm-clock/src/db.rs|cargo test -p alarm-clock"
"10|mopidy-transport|4.1,4.2|D4|mopidy-client>Reconnecting WebSocket JSON-RPC client;mopidy-client>Indefinite reconnect with bounded backoff|mopidy-client/src/lib.rs|cargo test -p mopidy-client"
"11|connection-state|4.3|D4|mopidy-client>Connection-state signal|mopidy-client/src/transport.rs;mopidy-client/src/reconnect.rs;alarm-clock/src/chan.rs|cargo test -p mopidy-client"
"12|typed-methods|4.4|D4|mopidy-client>Typed minimal method surface|mopidy-client/src/transport.rs|cargo test -p mopidy-client"
"13|event-parsing|4.5|D4|mopidy-client>Event channel|mopidy-client/src/transport.rs;alarm-clock/src/chan.rs|cargo test -p mopidy-client"
"14|mopidy-e2e|4.6|D4|mopidy-client>Reconnecting WebSocket JSON-RPC client;mopidy-client>Connection-state signal;mopidy-client>Typed minimal method surface;mopidy-client>Event channel|mopidy-client/src/|MANUAL"
"15|slint-nav|5.1,5.2|D9|ui-shell>Slint application with vertical orientation;ui-shell>Multi-panel navigation scaffold|alarm-clock/src/main.rs;alarm-clock/src/runtime.rs|cargo check -p alarm-clock"
"16|clock-panel|5.3,5.4|D9|ui-shell>Clock panel with reserved theme seam|alarm-clock/ui/main.slin|cargo check -p alarm-clock"
"17|systemd-unit|6.1|D10|process-runtime>systemd Type=notify readiness|alarm-clock/src/main.rs;alarm-clock/src/shutdown.rs|systemd-analyze verify dist/alarm-clock.service"
"18|pi-acceptance|6.2,6.3,6.4,6.5,6.6|D10,D5|process-runtime>systemd Type=notify readiness;process-runtime>Graceful shutdown seam;process-runtime>Structured logging to journald;ui-shell>Slint application with vertical orientation|alarm-clock/src/|MANUAL"
)

# ─── Extractors (header-anchor based; never hand-quoted) ───────────────────

extract_design() {
  local ids="$1"
  [[ -z "$ids" ]] && return 0
  local id
  for id in ${ids//,/ }; do
    awk -v id="$id" '
      /^## / { if (insec) exit; else next }
      /^### D[0-9]+\./ {
        match($0, /D[0-9]+/);
        cur = substr($0, RSTART, RLENGTH);
        if (cur == id) { insec=1; print; next }
        else { if (insec) exit }
        next
      }
      insec { print }
    ' "$DESIGN"
  done
}

extract_spec() {
  local refs="$1"
  [[ -z "$refs" ]] && return 0
  local ref cap name file
  for ref in ${refs//;/ }; do
    cap="${ref%%>*}"
    name="${ref#*>}"
    file="$SPECS/$cap/spec.md"
    [[ -f "$file" ]] || { echo "WARN: spec file not found: $file" >&2; continue; }
    awk -v name="$name" '
      /^## / { if (insec) exit; else next }
      /^### Requirement: / {
        if (index($0, "Requirement: " name) > 0) { insec=1; print }
        else { if (insec) exit }
        next
      }
      insec { print }
    ' "$file"
  done
}

extract_tasks() {
  local ids="$1"
  grep -E "^- \[[ x]\] (${ids//,/|}) " "$TASKS" || true
}

files_block() {
  local f="$1"
  [[ -z "$f" ]] && { echo "none — greenfield"; return; }
  echo "$f" | tr ';' '\n' | sed 's/^/  - /'
}

# ─── Helpers ───────────────────────────────────────────────────────────────

all_tasks_done() {
  local ids="$1" id
  for id in ${ids//,/ }; do
    grep -qE "^- \[x\] $id " "$TASKS" || return 1
  done
  return 0
}

build_impl_prompt() {
  local num="$1" name="$2" taskids="$3" design="$4" specs="$5" files="$6" verify="$7"
  local design_text spec_text task_lines
  design_text="$(extract_design "$design")"
  spec_text="$(extract_spec "$specs")"
  task_lines="$(extract_tasks "$taskids")"
  cat <<EOF
You are implementing ONE task group in a Rust alarm-clock project (slice 0 of an OpenSpec change). Work in the current directory: $ROOT

PROJECT ROOT: $ROOT

TASKS (do exactly these, nothing else — implement ONLY these task ids):
$task_lines

DESIGN CONTEXT (the architecture you must follow for these tasks):
${design_text:-(none — infrastructure task; no design excerpt needed)}

SPEC REQUIREMENTS (the acceptance criteria; scenarios are your tests):
${spec_text:-(none for this group)}

EXISTING CODE (read these files before writing; modify only what these tasks require):
$(files_block "$files")

INSTRUCTIONS:
- Make minimal, focused changes for ONLY the tasks above.
- Do NOT implement other tasks, future groups, or speculate about later slices.
- After completing each task, mark its checkbox in $TASKS : change "- [ ]" to "- [x]" for that task id. Use the edit tool on that exact line.
- Use bash, read, edit, write tools as needed.
- Before finishing, run this VERIFY command via the bash tool and ensure it passes. If it fails, fix and re-run until it passes:
    $verify
- When done: confirm verify passed and checkboxes are flipped, then STOP. Do not continue to other tasks.
EOF
}

build_val_prompt() {
  local num="$1" name="$2" taskids="$3" specs="$4" files="$5" verify="$6"
  local spec_text task_lines
  spec_text="$(extract_spec "$specs")"
  task_lines="$(extract_tasks "$taskids")"
  cat <<EOF
You are VALIDATING one task group in a Rust alarm-clock project (slice 0). Work in: $ROOT

You are reviewing the work another session just did for this group. Do NOT implement features. Only verify.

TASKS that should now be done:
$task_lines

ACCEPTANCE CRITERIA (spec scenarios):
${spec_text:-(none for this group)}

EXPECTED TOUCHED FILES (the diff should be limited to these plus $TASKS ):
$(files_block "$files")

VERIFY COMMAND (run it via the bash tool; it MUST exit 0):
    $verify

CHECKS TO PERFORM:
1. Run the VERIFY command. It MUST pass (exit 0).
2. Run \`git diff --stat\` AND \`git status --short\` at the repo root. Confirm changes are limited to the expected files plus $TASKS . Unrelated files = scope violation.
3. Read $TASKS and confirm the checkboxes for this group's task ids are now "- [x]".
4. Skim the diff for obvious bugs, missing error handling, or deviations from the SPEC above.

OUTPUT:
End your response with EXACTLY one final line of the form:
    VERDICT: PASS
or
    VERDICT: FAIL
Precede it with a detailed diagnosis if failing, so the next implementer can fix every issue. List each concrete problem with file paths and line numbers where possible.
If the VERIFY command fails, any checkbox is still [ ], or scope is violated, you MUST output VERDICT: FAIL.
EOF
}

build_repair_prompt() {
  local num="$1" name="$2" taskids="$3" design="$4" specs="$5" files="$6" verify="$7"
  local prev_val_output="$8"
  local design_text spec_text task_lines
  design_text="$(extract_design "$design")"
  spec_text="$(extract_spec "$specs")"
  task_lines="$(extract_tasks "$taskids")"
  cat <<EOF
You are REPAIRING one task group in a Rust alarm-clock project (slice 0 of an OpenSpec change). Work in the current directory: $ROOT

PROJECT ROOT: $ROOT

TASKS (fix ONLY these task ids, do not regress completed work):
$task_lines

DESIGN CONTEXT:
${design_text:-(none — infrastructure task; no design excerpt needed)}

SPEC REQUIREMENTS:
${spec_text:-(none for this group)}

EXISTING CODE:
$(files_block "$files")

VERIFY COMMAND (run via the bash tool; it MUST exit 0 when you are done):
    $verify

---

PREVIOUS VALIDATOR OUTPUT (this is the feedback you must address — fix every issue below):
=======================================================================
$prev_val_output
=======================================================================

INSTRUCTIONS:
- Read the validator output above carefully. Fix EVERY problem it identifies.
- After fixing, run the VERIFY command via the bash tool and ensure it passes.
- If checkboxes were not flipped in $TASKS , flip them now ("- [ ]" to "- [x]" for this group's task ids). Use the edit tool on those exact lines.
- Make minimal, focused changes. Do NOT implement other groups' tasks.
- Before finishing, run the VERIFY command and confirm it passes.
EOF
}

run_group() {
  local line="$1"
  local num name taskids design specs files verify
  IFS='|' read -r num name taskids design specs files verify <<< "$line"

  echo
  echo "════════════════════════════════════════════════════════════════"
  echo "  GROUP $num — $name"
  echo "  tasks: $taskids   verify: $verify"
  echo "════════════════════════════════════════════════════════════════"

  if all_tasks_done "$taskids"; then
    echo "  ✓ all task boxes already [x] — skipping (resume-safe)"
    return 0
  fi

  if [[ "$verify" == "MANUAL" ]]; then
    echo "  ⏸  MANUAL group — not run automatically. Run interactively:"
    echo "     pi -p --no-skills --no-context-files -a --model $PI_MODEL--name slice0-G$num-impl < $PROMPTDIR/G$num-impl.txt"
    build_impl_prompt "$num" "$name" "$taskids" "$design" "$specs" "$files" "<run the manual checklist from the manifest>" > "$PROMPTDIR/G$num-impl.txt"
    echo "     (prompt written to $PROMPTDIR/G$num-impl.txt)"
    return 0
  fi

  # ── Mini ralph-loop: implement → validate → repair (with validator output) → ... ──
  local iter=1
  while [[ $iter -le $MAX_ITER ]]; do
    local impl_log="$LOGDIR/G$num-impl-${iter}.log"
    local val_log="$LOGDIR/G$num-val-${iter}.log"

    if [[ $iter -eq 1 ]]; then
      # First iteration: fresh implementation
      build_impl_prompt "$num" "$name" "$taskids" "$design" "$specs" "$files" "$verify" > "$PROMPTDIR/G$num-impl.txt"
      echo "  → [iter $iter/$MAX_ITER] implementing (session: slice0-G$num-impl-$iter)..."
      if ! "${PI[@]}" --name "slice0-G$num-impl-$iter" < "$PROMPTDIR/G$num-impl.txt" > "$impl_log" 2>&1; then
        echo "  ✗ pi implementer exited non-zero (iter $iter). See $impl_log"
        exit 1
      fi
      echo "  · [iter $iter/$MAX_ITER] implementer done. log: $impl_log"
    else
      # Subsequent iterations: repair with validator feedback from previous iteration
      local prev_val_output
      prev_val_output="$(cat "$LOGDIR/G$num-val-$((iter - 1)).log")"
      build_repair_prompt "$num" "$name" "$taskids" "$design" "$specs" "$files" "$verify" "$prev_val_output" > "$PROMPTDIR/G$num-repair-${iter}.txt"
      echo "  → [iter $iter/$MAX_ITER] repairing with validator feedback (session: slice0-G$num-repair-$iter)..."
      if ! "${PI[@]}" --name "slice0-G$num-repair-$iter" < "$PROMPTDIR/G$num-repair-${iter}.txt" > "$impl_log" 2>&1; then
        echo "  ✗ pi repairer exited non-zero (iter $iter). See $impl_log"
        exit 1
      fi
      echo "  · [iter $iter/$MAX_ITER] repairer done. log: $impl_log"
    fi

    # Validate this iteration's work
    build_val_prompt "$num" "$name" "$taskids" "$specs" "$files" "$verify" > "$PROMPTDIR/G$num-val-${iter}.txt"
    echo "  → [iter $iter/$MAX_ITER] validating (session: slice0-G$num-val-$iter)..."
    if ! "${PI[@]}" --name "slice0-G$num-val-$iter" < "$PROMPTDIR/G$num-val-${iter}.txt" > "$val_log" 2>&1; then
      echo "  ✗ pi validator exited non-zero (iter $iter). See $val_log"
      exit 1
    fi
    local verdict
    verdict="$(grep -oE "VERDICT: (PASS|FAIL)" "$val_log" | tail -1 || true)"
    echo "  · [iter $iter/$MAX_ITER] validator: $verdict"

    if [[ "$verdict" == "VERDICT: PASS" ]]; then
      # Commit clean baseline and break out of loop
      git -C "$ROOT" add -A
      git -C "$ROOT" commit -q -m "slice0 G$num: $name (iterated $iter)" || echo "  · nothing to commit"
      echo "  ✓ PASS after $iter iteration(s) — committed"
      return 0
    fi

    # Did not pass; continue to next iteration if we haven't exhausted retries
    if [[ $iter -ge $MAX_ITER ]]; then
      echo "  ✗ VALIDATION FAILED after $MAX_ITER iterations. Stopping."
      echo "    val log:   $val_log"
      echo "    val prompt:$PROMPTDIR/G$num-val-${iter}.txt"
      return 1
    fi
    iter=$((iter + 1))
  done
}

# ─── Main ──────────────────────────────────────────────────────────────────

command -v pi >/dev/null 2>&1 || { echo "error: pi not found on PATH"; exit 1; }
[[ -f "$TASKS" ]] || { echo "error: tasks.md not found at $TASKS"; exit 1; }

# Optional positional args = explicit group numbers to run (e.g. ./apply-loop.sh 4 7)
if [[ $# -gt 0 ]]; then
  for want in "$@"; do
    # Normalize to 2-digit zero-padded string for comparison with TASK_GROUPS
    want="$(printf "%02d" "$want" 2>/dev/null || echo "$want")"
    found=false
    for line in "${TASK_GROUPS[@]}"; do
      IFS='|' read -r num name taskids design specs files verify <<< "$line"
      if [[ "$num" == "$want" ]]; then
        run_group "$line"
        found=true
        break
      fi
    done
    if [[ "$found" == "false" ]]; then
      echo "ERROR: group $want not found in TASK_GROUPS" >&2
      exit 1
    fi
  done
else
  for line in "${TASK_GROUPS[@]}"; do run_group "$line"; done
fi

echo
echo "════════════════════════════════════════════════════════════════"
echo "  Done. Manual groups (14, 18) skipped — run interactively."
echo "════════════════════════════════════════════════════════════════"
