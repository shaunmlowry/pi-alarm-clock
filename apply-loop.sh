#!/usr/bin/env bash
#
# apply-loop.sh — drive an OpenSpec change's implementation group-by-group through pi.
#
# Usage:
#   ./apply-loop.sh <change> [group-numbers...] [--dry-run|-n]
#
#   <change>            change dir name under openspec/changes/
#   group-numbers       optional: run only these groups (e.g. 1 3 5); default = all
#   --dry-run / -n      build prompts + print the plan, do not invoke pi
#
# Per group (three-layer gating, with model escalation on retry):
#   Up to 5 attempts. Attempts 1–3 use $PRIMARY_MODEL; attempts 4–5 use
#   $ESCALATION_MODEL. Each attempt runs the full three-layer gauntlet:
#   1. IMPLEMENTER (fresh pi session): scoped prompt → does the work, runs the
#      verify command, fixes until it passes, flips task checkboxes in tasks.md.
#   2. HARD GATE (bash, the reliable source of truth): re-runs the verify command
#      and greps that every task checkbox is now [x]. Fail → retry next attempt.
#   3. VALIDATOR (fresh pi session): qualitative review — git-diff scope, spec
#      match, obvious bugs — and emits `VERDICT: PASS`/`VERDICT: FAIL`. Does NOT
#      re-run cargo (the hard gate already did). Fail → retry next attempt.
#   4. On PASS: git commit so the next group sees a clean baseline.
#   Between attempts the group's declared files + tasks.md are reset to the last
#   commit (un-flipping checkboxes, reverting partial code) so each attempt
#   starts clean — without touching unrelated untracked files.
#   If all 5 attempts fail, the loop stops.
#
# MANUAL groups (verify=MANUAL): the implementer prompt is written to a file and
# the exact pi command is printed; the script skips the gate/val/commit and
# continues. Resume-safe: groups whose task boxes are all [x] are skipped.
#
# Model escalation is env-overridable:
#   PRIMARY_MODEL="..." PRIMARY_ATTEMPTS=3 \
#   ESCALATION_MODEL="..." ESCALATION_ATTEMPTS=2 \
#   PI_PROVIDER=ollama ./apply-loop.sh <change> ...
# Freshness: pi is invoked with -p --no-skills --no-context-files -a and NO
# -c/-r/--session, so every invocation starts with an empty context window.
# --name saves each session for audit but does NOT load it into the next run.
#
# Groups are read from openspec/changes/<change>/apply-groups.tsv (pipe-delimited;
# see the header comment in that file for the field schema).

set -euo pipefail

# ─── Resolve paths ──────────────────────────────────────────────────────────
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHANGES_DIR="$ROOT/openspec/changes"

# ─── Parse args ─────────────────────────────────────────────────────────────
if [[ $# -lt 1 ]]; then
  echo "usage: $0 <change> [group-numbers...] [--dry-run|-n]" >&2
  exit 2
fi
CHANGE_NAME="$1"; shift
CHANGE="$CHANGES_DIR/$CHANGE_NAME"
DESIGN="$CHANGE/design.md"
SPECS="$CHANGE/specs"
TASKS="$CHANGE/tasks.md"
GROUPS_FILE="$CHANGE/apply-groups.tsv"
APPLYDIR="$CHANGE/.apply"
PROMPTDIR="$APPLYDIR/prompts"
LOGDIR="$APPLYDIR/logs"

DRY_RUN=0
SELECTED=()
for a in "$@"; do
  case "$a" in
    --dry-run|-n) DRY_RUN=1 ;;
    -*) echo "unknown flag: $a" >&2; exit 2 ;;
    *) SELECTED+=("$(printf '%02d' "$a")") ;;
  esac
done

mkdir -p "$PROMPTDIR" "$LOGDIR"

# ─── Model escalation config (env-overridable) ──────────────────────────────
# Model strings use pi's "provider/id" prefix form so each model targets its
# own provider (Qwen is local via ollama; GLM-5.2 via openrouter). No --provider
# flag is passed unless PI_PROVIDER is explicitly set.
PRIMARY_MODEL="${PRIMARY_MODEL:-ollama/erbanku/Qwen3-Coder-Next-GGUF-UD-IQ4_XS:latest}"
PRIMARY_ATTEMPTS="${PRIMARY_ATTEMPTS:-3}"
ESCALATION_MODEL="${ESCALATION_MODEL:-openrouter/z-ai/glm-5.2}"
ESCALATION_ATTEMPTS="${ESCALATION_ATTEMPTS:-2}"
PI_PROVIDER="${PI_PROVIDER:-}"   # optional override; if set, passed as --provider
TOTAL_ATTEMPTS=$((PRIMARY_ATTEMPTS + ESCALATION_ATTEMPTS))

# Return the model id for a given 1-based attempt number.
attempt_model() {
  local n="$1"
  if (( n <= PRIMARY_ATTEMPTS )); then echo "$PRIMARY_MODEL"
  else echo "$ESCALATION_MODEL"; fi
}

# Build the pi base args for a given model (print mode, scoped, trust project).
# Sets the global PI_ARGS array (caller appends --name etc.).
build_pi_args() {
  local model="$1"
  PI_ARGS=(pi -p --no-skills --no-context-files -a --model "$model")
  [[ -n "$PI_PROVIDER" ]] && PI_ARGS+=(--provider "$PI_PROVIDER")
  return 0
}

cd "$ROOT"

# ─── Pre-flight checks ──────────────────────────────────────────────────────
require() { [[ -f "$1" ]] || { echo "error: not found: $1" >&2; exit 1; } }
require "$TASKS"
require "$DESIGN"
require "$GROUPS_FILE"
if [[ "$DRY_RUN" -eq 0 ]]; then
  command -v pi >/dev/null 2>&1 || { echo "error: pi not found on PATH" >&2; exit 1; }
fi

# ─── Extractors (header-anchor based; never hand-quoted) ────────────────────
# Pull a design decision by its D-id anchor (### D<n>. ...).
extract_design() {
  local ids="$1"
  [[ -z "$ids" ]] && return 0
  local id
  local IFS=','
  for id in $ids; do
    awk -v id="$id" '
      /^## / { if (insec) exit; else next }
      /^### D[0-9]+\./ {
        match($0, /D[0-9]+/);
        cur = substr($0, RSTART, RLENGTH);
        if (cur == id) { insec=1; print; next }
        else if (insec) exit
        next
      }
      insec { print }
    ' "$DESIGN"
  done
}

# Pull a spec requirement by its exact heading (### Requirement: <name>).
extract_spec() {
  local refs="$1"
  [[ -z "$refs" ]] && return 0
  local ref cap name file
  local IFS=';'
  for ref in $refs; do
    cap="${ref%%>*}"
    name="${ref#*>}"
    file="$SPECS/$cap/spec.md"
    [[ -f "$file" ]] || { echo "WARN: spec file not found: $file" >&2; continue; }
    awk -v name="$name" '
      /^## / { if (insec) exit; else next }
      /^### Requirement: / {
        if (index($0, "Requirement: " name) > 0) { insec=1; print }
        else if (insec) exit
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
  [[ -z "$f" ]] && { echo "  (none — greenfield / new file)"; return; }
  echo "$f" | tr ';' '\n' | sed 's/^/  - /'
}

# ─── Helpers ────────────────────────────────────────────────────────────────
all_tasks_done() {
  local ids="$1" id
  local IFS=','
  for id in $ids; do
    grep -qE "^- \[x\] $id " "$TASKS" || return 1
  done
  return 0
}

any_task_undone() { ! all_tasks_done "$1"; }

# Hard gate: run the verify command in bash (repo root) AND confirm checkboxes.
# Args: <verify> <taskids> <log-path>. Returns 0 only if both pass.
hard_gate() {
  local verify="$1" taskids="$2" logpath="$3"
  if ! bash -c "$verify" >"$logpath" 2>&1; then
    echo "  ✗ HARD GATE: verify command failed: $verify"
    echo "    log: $logpath"
    return 1
  fi
  if any_task_undone "$taskids"; then
    echo "  ✗ HARD GATE: not all task boxes are [x] for: $taskids"
    local id
    local IFS=','
    for id in $taskids; do
      grep -qE "^- \[x\] $id " "$TASKS" || echo "    still [ ]: $id"
    done
    return 1
  fi
  echo "  · hard gate: verify passed, all boxes [x]"
  return 0
}

# Reset a group's workspace to the last commit before a retry: un-flip tasks.md
# checkboxes and revert/remove the group's declared files. Does NOT touch
# unrelated untracked files (only paths in $files + $TASKS).
reset_group_workspace() {
  local files="$1"
  # tasks.md is tracked (committed at planning time) → restore checkboxes.
  git -C "$ROOT" checkout -- "$TASKS" 2>/dev/null || true
  local f
  local IFS=';'
  for f in $files; do
    [[ -z "$f" ]] && continue
    if git -C "$ROOT" ls-files --error-unmatch "$f" >/dev/null 2>&1; then
      git -C "$ROOT" checkout -- "$f" 2>/dev/null || true   # tracked → restore
    else
      rm -f "$ROOT/$f" 2>/dev/null || true                   # new (untracked) → remove
    fi
  done
}

# ─── Prompt builders ────────────────────────────────────────────────────────
build_impl_prompt() {
  local num="$1" name="$2" taskids="$3" design="$4" specs="$5" files="$6" verify="$7"
  local design_text spec_text task_lines
  design_text="$(extract_design "$design")"
  spec_text="$(extract_spec "$specs")"
  task_lines="$(extract_tasks "$taskids")"
  cat <<EOF
You are implementing ONE task group in a Rust alarm-clock project (OpenSpec change: $CHANGE_NAME). Work in: $ROOT

PROJECT ROOT: $ROOT

TASKS (implement ONLY these task ids — nothing else):
$task_lines

DESIGN CONTEXT (architecture you must follow for these tasks):
${design_text:-(none for this group)}

SPEC REQUIREMENTS (acceptance criteria; scenarios are your tests):
${spec_text:-(none for this group)}

EXISTING CODE (read these before writing; modify only what these tasks require):
$(files_block "$files")

INSTRUCTIONS:
- Make minimal, focused changes for ONLY the tasks above. Do not implement other tasks, future groups, or speculate about later slices.
- After completing EACH task, mark its checkbox in $TASKS: change "- [ ]" to "- [x]" for that task id (use the edit tool on that exact line).
- Use bash, read, edit, write tools as needed. Follow existing code conventions in the files you read.
- Before finishing, run this VERIFY command via the bash tool and ensure it passes (exit 0). If it fails, fix and re-run until it passes:
    $verify
- When done: confirm the verify command passed and the checkboxes are flipped, then STOP. Do not continue to other tasks.
EOF
}

build_val_prompt() {
  local num="$1" name="$2" taskids="$3" specs="$4" files="$5" verify="$6"
  local spec_text task_lines
  spec_text="$(extract_spec "$specs")"
  task_lines="$(extract_tasks "$taskids")"
  cat <<EOF
You are VALIDATING one task group in a Rust alarm-clock project (OpenSpec change: $CHANGE_NAME). Work in: $ROOT

You are reviewing the work another session just did. Do NOT implement features — only verify. The hard gate (cargo verify command + checkbox check) has ALREADY PASSED — do NOT re-run cargo. Focus on qualitative review.

TASKS that should now be done:
$task_lines

ACCEPTANCE CRITERIA (spec scenarios — check the code actually satisfies these):
${spec_text:-(none for this group)}

EXPECTED TOUCHED FILES (these are the primary files expected for this group, but adjacent changes may occur):
$(files_block "$files")

CHECKS TO PERFORM:
1. Run \`git diff --stat\` AND \`git status --short\` at the repo root. Note any files outside the expected set — these may be adjacent changes or preparatory work for future groups.
2. Read $TASKS and confirm the checkboxes for this group's task ids are "- [x]".
3. Skim the diff for: obvious bugs, missing error handling, deviations from the SPEC scenarios above, code that doesn't actually satisfy a scenario's THEN clause.
4. Flag any unexpected changes that suggest work from other task groups has been accidentally included (e.g., unchecked tasks in tasks.md).

OUTPUT:
End your response with EXACTLY one final line:
    VERDICT: PASS
or
    VERDICT: FAIL
Precede it with a one-line reason. Example: "All checks passed. VERDICT: PASS"
Only output VERDICT: FAIL if the group's own tasks are not complete, spec scenarios are not satisfied, or there are obvious bugs.
EOF
}

# ─── Per-group runner ───────────────────────────────────────────────────────
run_group() {
  local line="$1"
  local num name taskids design specs files verify
  IFS='|' read -r num name taskids design specs files verify <<< "$line"

  echo
  echo "════════════════════════════════════════════════════════════════"
  echo "  GROUP $num — $name"
  echo "  tasks: $taskids"
  echo "  verify: $verify"
  echo "  escalation: $PRIMARY_MODEL ×$PRIMARY_ATTEMPTS → $ESCALATION_MODEL ×$ESCALATION_ATTEMPTS"
  echo "════════════════════════════════════════════════════════════════"

  if all_tasks_done "$taskids"; then
    echo "  ✓ all task boxes already [x] — skipping (resume-safe)"
    return 0
  fi

  if [[ "$verify" == "MANUAL" ]]; then
    build_impl_prompt "$num" "$name" "$taskids" "$design" "$specs" "$files" "(manual — follow the task's own acceptance steps)" > "$PROMPTDIR/G$num-impl.txt"
    echo "  ⏸  MANUAL group — not run automatically. To run interactively:"
    echo "     pi -p --no-skills --no-context-files -a --name ${CHANGE_NAME}-G${num}-impl < $PROMPTDIR/G$num-impl.txt"
    echo "     (prompt written to $PROMPTDIR/G$num-impl.txt)"
    return 0
  fi

  # Build the (model-independent) prompts once.
  build_impl_prompt "$num" "$name" "$taskids" "$design" "$specs" "$files" "$verify" > "$PROMPTDIR/G$num-impl.txt"
  build_val_prompt  "$num" "$name" "$taskids" "$specs" "$files" "$verify" > "$PROMPTDIR/G$num-val.txt"

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "  [dry-run] would attempt up to $TOTAL_ATTEMPTS times (3×$PRIMARY_MODEL, 2×$ESCALATION_MODEL)"
    echo "  [dry-run] impl prompt: $PROMPTDIR/G$num-impl.txt"
    echo "  [dry-run] val prompt:  $PROMPTDIR/G$num-val.txt"
    return 0
  fi

  local attempt model impl_log val_log gate_log
  for ((attempt=1; attempt<=TOTAL_ATTEMPTS; attempt++)); do
    model="$(attempt_model "$attempt")"
    impl_log="$LOGDIR/G${num}-a${attempt}-impl.log"
    val_log="$LOGDIR/G${num}-a${attempt}-val.log"
    gate_log="$LOGDIR/G${num}-a${attempt}-gate.log"
    build_pi_args "$model"

    if (( attempt > 1 )); then
      echo "  ↺ retry: resetting workspace to last commit before attempt $attempt"
    fi
    reset_group_workspace "$files"
    echo "  → attempt $attempt/$TOTAL_ATTEMPTS (model: $model)"

    # 1. Implementer
    echo "    · implementing (session: ${CHANGE_NAME}-G${num}-a${attempt}-impl)..."
    if ! "${PI_ARGS[@]}" --name "${CHANGE_NAME}-G${num}-a${attempt}-impl" \
        < "$PROMPTDIR/G$num-impl.txt" > "$impl_log" 2>&1; then
      echo "    ✗ attempt $attempt: implementer exited non-zero → retry"
      echo "      log: $impl_log"
      continue
    fi

    # 2. Hard gate (bash — reliable source of truth)
    if ! hard_gate "$verify" "$taskids" "$gate_log"; then
      echo "    ✗ attempt $attempt: hard gate failed → retry"
      echo "      impl log: $impl_log"
      continue
    fi

    # 3. Validator (fresh session, qualitative only)
    echo "    · validating (session: ${CHANGE_NAME}-G${num}-a${attempt}-val)..."
    if ! "${PI_ARGS[@]}" --name "${CHANGE_NAME}-G${num}-a${attempt}-val" \
        < "$PROMPTDIR/G$num-val.txt" > "$val_log" 2>&1; then
      echo "    ✗ attempt $attempt: validator exited non-zero → retry"
      echo "      log: $val_log"
      continue
    fi
    local verdict
    verdict="$(grep -oE "VERDICT: (PASS|FAIL)" "$val_log" | tail -1 || true)"
    echo "    · validator: ${verdict:-<no verdict line>}"
    if [[ "$verdict" != "VERDICT: PASS" ]]; then
      echo "    ✗ attempt $attempt: validator ${verdict:-no verdict} → retry"
      echo "      impl log: $impl_log"
      echo "      val log:  $val_log"
      continue
    fi

    # 4. PASS — commit clean baseline
    git -C "$ROOT" add -A
    if git -C "$ROOT" diff --cached --quiet; then
      echo "  ✓ PASS on attempt $attempt ($model) — nothing to commit (working tree clean)"
    else
      git -C "$ROOT" commit -q -m "${CHANGE_NAME} G${num}: ${name} (attempt ${attempt}, ${model})"
      echo "  ✓ PASS on attempt $attempt ($model) — committed"
    fi
    return 0
  done

  echo "  ✗ GROUP $num FAILED after $TOTAL_ATTEMPTS attempts. Stopping the loop."
  echo "    prompts: $PROMPTDIR/G$num-impl.txt, $PROMPTDIR/G$num-val.txt"
  echo "    last impl log: $LOGDIR/G${num}-a${TOTAL_ATTEMPTS}-impl.log"
  echo "    last val log:  $LOGDIR/G${num}-a${TOTAL_ATTEMPTS}-val.log"
  return 1
}

# ─── Main ───────────────────────────────────────────────────────────────────
mapfile -t GROUP_LINES < <(grep -vE '^[[:space:]]*(#|$)' "$GROUPS_FILE")

if [[ ${#GROUP_LINES[@]} -eq 0 ]]; then
  echo "error: no groups in $GROUPS_FILE" >&2
  exit 1
fi

failed=0
for line in "${GROUP_LINES[@]}"; do
  num="${line%%|*}"
  # If specific groups requested, skip non-matches.
  if [[ ${#SELECTED[@]} -gt 0 ]]; then
    match=false
    for s in "${SELECTED[@]}"; do [[ "$num" == "$s" ]] && match=true && break; done
    [[ "$match" == true ]] || continue
  fi
  if ! run_group "$line"; then
    failed=1
    break
  fi
done

echo
echo "════════════════════════════════════════════════════════════════"
if [[ "$failed" -eq 1 ]]; then
  echo "  STOPPED on failure. Fix the issue and re-run to resume."
else
  echo "  Done. MANUAL groups (14, 15, 17) skipped — run interactively."
fi
echo "════════════════════════════════════════════════════════════════"
[[ "$failed" -eq 0 ]]
