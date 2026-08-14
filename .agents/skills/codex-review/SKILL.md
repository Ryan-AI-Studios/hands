---
name: codex-review
description: >
  Cross-model track completion audit for Helping Hands after implementation and internal review
  fixes. Verifies every DoD item, finds placeholders, incomplete wiring, regressions, and weak
  evidence. Read-only. Orchestrator fixes and re-invokes until nothing above low remains; final
  gate requires a fresh clean pass. Use when implement skill reaches the codex gate or user asks
  for codex/cross-model track review.
---

# Track Completion Review (Cross-Model) — Helping Hands

Read-only audit. The **orchestrator** (implement skill) selects the track, implements, fixes,
runs gates, manages `deferred.md`, and decides completion.

## Handoff (required)

```text
TRACK: <####-Name or absolute track directory under C:\dev\Helping-Hands\conductor>
```

Optional:

```text
REPOS: C:\dev\Helping-Hands\hands
SCOPE: <base/commit range/working tree/PR>
IMPLEMENTED: <brief summary>
KNOWN GATES: <cargo/CI results observed>
FOCUS: <extra risks>
```

```text
ROOT=C:\dev\Helping-Hands
DEFERRED=C:\dev\Helping-Hands\conductor\deferred.md
PRODUCT=C:\dev\Helping-Hands\hands
```

Raw output: `C:\dev\Helping-Hands\conductor\<track>\review.codex.md` (or `review.claude.md`).
Orchestrator writes canonical `review.md`.

## Rules

* **Never** modify product files, governance, Git state, or `deferred.md`.
* Read every requirement, plan phase, risk, and DoD item.
* Do not claim a command passed unless observed (or honestly “reported by orchestrator”).
* No invented or style-only findings.
* Do not overturn locked SHARED-UNDERSTANDING; flag product questions for the owner.
* Planning markdown must **not** have been committed under product.

## Product context

* **Mission:** Windows eyes-and-hands MCP/CLI for personal research on daily Chrome.
* **Stack:** Rust-first; fused observe; OS SendInput; no CDP.
* **Tools:** ledgerful + ai-brains belong in product tree when used.
* **Non-goals:** Playwright/CDP, CAPTCHA solvers, HID, injected-flag spoofing.

## Audit sections

1. Requirements / DoD / plan fidelity matrix
2. Completeness sweep (TODO/FIXME/stub/placeholder/fake success)
3. Wiring (end-to-end for claimed behavior)
4. Correctness / regression / safety (confirm fence, Pause/Break, no CDP)
5. Tests / evidence honesty
6. Docs / governance (no planning-in-product)

## Severity map

| Reviewer | Implement | Deferrable? |
|----------|-----------|-------------|
| P0 | critical | No |
| P1 | high | No |
| P2 | medium | No |
| P3 | low | Yes if difficult / non-DoD |

## Output template

```text
# Track Completion Audit — <TRACK>
## Verdict: PASS | PASS WITH DEFERRED P3 | FAIL
## Scope Reviewed
## Requirement and DoD Matrix
## Findings
## Completeness Sweep
## Wiring and Regression Review
## Verification Evidence
## Deferred Candidates
## Completion Decision
```

## Reviewers (cross-model order)

1. **Codex Primary**
2. **Claude Secondary** when Codex unavailable
3. Optional OpenCode tertiary

### Codex Primary (Windows)

```powershell
$TrackDir = "C:\dev\Helping-Hands\conductor\<####-Name>"
$PrimaryRepo = "C:\dev\Helping-Hands\hands"
$Prompt = @"
You are the independent completion reviewer for Helping Hands track <TRACK>.
Track directory: $TrackDir
Product repo: $PrimaryRepo
Planning root (read-only): C:\dev\Helping-Hands
READ-ONLY; never modify files or Git.
Audit every DoD against implementation. Flag planning docs committed into product.
"@

codex exec -C $PrimaryRepo -s read-only `
  -m gpt-5.4 -c 'model_reasoning_effort="high"' `
  --add-dir "C:\dev\Helping-Hands" --ephemeral `
  -o "$TrackDir\review.codex.md" $Prompt
```

### Ledgerful under pure RO

Optional `ledgerful ledger status --json` / `change-context --json` if `.ledgerful` exists.
**Skip** `doctor` / `index` / `scan --impact` / `verify` inside pure RO review.

## Orchestrator loop (after return)

1. Classify findings
2. Fix validated P0–P2 and easy P3
3. Record qualifying difficult P3/lows in `deferred.md`
4. Re-run internal review as needed
5. **Re-invoke this skill** for a **fresh** pass
6. Final gate = last cross-model clean of >low
