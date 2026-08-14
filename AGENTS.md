# AGENTS.md — Hands (product)

Rust-first Windows eyes-and-hands MCP/CLI. **This directory is the product git root.**

Planning, conductor tracks, ADRs, and shared understanding live **one level up**:

`C:\dev\Helping-Hands\` (not in this repo’s commits).

Remote: https://github.com/Ryan-AI-Studios/hands

## Workspace split

| Path | Role |
|------|------|
| `C:\dev\Helping-Hands\hands\` | **This repo** — product code only |
| `C:\dev\Helping-Hands\` (except this folder) | Planning docs, ADRs |
| `C:\dev\Helping-Hands\conductor\` | Track registry / specs / plans |

**Never** commit `conductor/`, `docs/adr/`, `SHARED-UNDERSTANDING.md`, or planner handoff into this repo.

## Tools (always product cwd)

Init once when the repo is ready:

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains context
ledgerful init
```

Every coding session (when inited):

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ledgerful doctor --json
ledgerful change-context --json
```

Prefer `ledgerful … --json` when parsing. See `.agents/skills/ledgerful` and `ai-brains`.

## Build / test

Crate ships in a later track. When present:

```powershell
cd C:\dev\Helping-Hands\hands
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Ledgerful verify steps (when configured) must match these real cargo commands.

## Code style

- Rust edition and formatting as set by `rustfmt` / project `Cargo.toml`
- Prefer small, testable modules
- No secrets in git
- No Playwright, CDP, `--remote-debugging-port`, or CAPTCHA solvers

## Agent entry points

| Intent | Skill |
|--------|--------|
| Orient | `.agents/skills/onboarding` |
| Implement track | `.agents/skills/implement` |
| Cross-model gate | `.agents/skills/codex-review` |
| Plan only | `C:\dev\Helping-Hands\.agents\skills\plan` |

## PR discipline

- Feature branch → PR → CI green → squash-merge (default for later tracks)
- Bootstrap track **0001** may allow direct push to `main` per track plan
- Do not busy-poll CI
- Do not force-push shared history without owner confirmation

## Review focus

- Plan fidelity vs `conductor/<track>/`
- Wrong cwd for ledgerful/ai-brains
- Planning files staged into product
- Confirm fence / Pause-Break / no-CDP invariants
