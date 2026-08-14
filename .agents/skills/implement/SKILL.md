---
name: implement
description: >
  Implement one assigned Helping Hands conductor track end-to-end in C:\dev\Helping-Hands\hands.
  Load with product onboarding first. Subagents implement/review loop; codex-review cross-model
  gate until nothing above low remains; fresh clean cross-model final gate. PR + CI green before
  squash-merge. Updates conductor.md and deferred.md. Use when the user says implement track,
  execute track, /implement.
---

# Implement Conductor Track — Helping Hands

identity{
  product: "Helping Hands"
  product_path: "C:\\dev\\Helping-Hands\\hands"
  planning_root: "C:\\dev\\Helping-Hands"
  conductor_root: "C:\\dev\\Helping-Hands\\conductor"
  load_with: "onboarding (product .agents/skills/onboarding)"
  source_of_truth: "conductor.md + <track>/spec.md + plan.md under C:\\dev\\Helping-Hands\\conductor"
  do_not:
    - clear gate with open critical/high/medium findings
    - commit planning docs into the product repo
    - invent product decisions
    - deviate from track plan/spec
    - mark Completed without review.md + conductor + deferred updates for unfinished lows
  must:
    - follow plan.md phases and spec DoD as written
    - update deferred.md at finish with every residual low not implemented
    - run ledgerful + ai-brains from PRODUCT cwd when inited
}

## When this skill applies

- `Implement track 0001` / `execute track 0001` / `/implement` + track id

**Not this skill:** `track N` / `/plan` alone; `Review track N` alone.

If track is not **Ready** or **In progress**, stop and report.

## Paths

```
PRODUCT=C:\dev\Helping-Hands\hands
CONDUCTOR=C:\dev\Helping-Hands\conductor
REGISTRY=C:\dev\Helping-Hands\conductor\conductor.md
DEFERRED=C:\dev\Helping-Hands\conductor\deferred.md
TRACK=C:\dev\Helping-Hands\conductor\<####-Name>
```

## Severity

| Severity | Gate |
|----------|------|
| critical / high / medium | **Block** completion |
| low | Fix if easy; else **APPEND** to `DEFERRED` |

Regression caused by this work is always **high**. Placeholder faking DoD is **blocking**.

## Standing orders

1. **Knowledge is stale** — re-verify APIs, crates live.
2. **Plan fidelity** — only spec + plan; stop if blocked.
3. **Deferred at finish (mandatory)** — every residual low → `deferred.md`.
4. **ledgerful + ai-brains** from **`PRODUCT` cwd only**.
5. **Docs-out-of-product** — never git-add planning paths.
6. **PR process** — branch → PR → CI green → squash-merge when policy allows.
7. **Mission** — advance Helping Hands (fused eyes/hands, not a scraper).

## Loop (end-to-end)

```
0  orient + deferred scan + tools preflight
1  mark In progress; branch
2  implementation brief from plan (orchestrator)
3  SUBAGENT implement
4  targeted checks (cargo/test as applicable)
5  SUBAGENT review vs DoD
6  fix → re-review until internal clean of >low (lows dispositioned)
7  CROSS-MODEL: codex-review (fresh)
8  FINAL GATE: clean cross-model with no open >low
9  PR + wait CI (no busy-poll) + squash-merge when green
10 write review.md; conductor Completed; deferred update
11 final report to owner
```

## Phase 0 — Orient

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ai-brains sync query "<track topic>"
ledgerful doctor --json
ledgerful ledger status --compact
ledgerful change-context --json
```

If tools not inited: continue; note in `review.md`. **Do not init in planning root.**

## Phase 1 — Branch

```powershell
cd C:\dev\Helping-Hands\hands
git fetch origin
git checkout main
git pull --ff-only
git checkout -b track/<####-short-name>
```

## Phase 2–3 — Implement + internal review (subagents)

- Edits only under `PRODUCT` (or named execution path).
- Orchestrator owns `conductor.md` / `deferred.md` / `review.md`.
- Implementer lists unfinished lows for deferred append.

Targeted checks:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# or track-specific commands from spec §8
```

## Phase 4 — Cross-model gate

Load **`codex-review`**. Any validated finding **above low** → fix → internal re-review → **fresh**
codex-review. Final gate must be a **new** clean pass.

## Phase 5 — PR + CI

```powershell
cd C:\dev\Helping-Hands\hands
git push -u origin HEAD
gh pr create --title "track(####): <short objective>" --body "..."
gh pr merge --squash --delete-branch   # when green + policy allows
```

## Phase 6 — Governance finalize

1. `TRACK\review.md` — DoD matrix, evidence, codex verdict, deferred lows list
2. `conductor.md` → **Completed**
3. **`deferred.md` mandatory append** of residual lows; resolve landed rows
4. Light-update `planner.md` if next-work snapshot changed
5. Optional: `ai-brains pin "DECISION: …"` from product cwd

## Anti-patterns

- Implementing without reading spec/plan
- Freestyle scope / next-track sneak-in
- Finishing without deferred.md append
- Skipping codex or reusing stale codex as final gate
- Busy-polling CI
- Committing planning tree into product
- Adding Playwright/CDP or a CAPTCHA solver

## Relation to other skills

| Skill | Role |
|-------|------|
| **onboarding** (product) | Orientation |
| **implement** (this) | Execute |
| **codex-review** | Cross-model completion audit |
| **plan** / **review-track** (planning tree) | Write/audit plans |
| **ledgerful** / **ai-brains** | Intelligence (product cwd) |
