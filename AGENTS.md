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

Tools are already inited in this directory. Every coding session:

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ledgerful doctor --json
# require environment.workRoot and stateDir under C:\dev\Helping-Hands\hands
ledgerful change-context --json
```

If doctor reports `workRoot: C:\dev\Helping-Hands`, you ran from the planning root — discard and
re-run from here. Re-run `ai-brains context` / `ledgerful init` only if `.env` / `.ledgerful` are
missing.

Prefer `ledgerful … --json` when parsing. See `.agents/skills/ledgerful` and `ai-brains`.

## Build / test

Crate is present. From this directory:

```powershell
cd C:\dev\Helping-Hands\hands
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CLI (same observe contract as MCP):

```powershell
cargo run -- mcp --help
cargo run -- observe --help
cargo run -- click --help
cargo run -- hover --help
cargo run -- type --help
cargo run -- key --help
cargo run -- scroll --help
cargo run -- wait-settle --help
cargo run -- stop --help
```

`hands mcp` is the stdio MCP server (`observe` plus `click` / `hover` / `type` / `key` / `scroll` / `wait_settle` / `stop`). `hands observe [--detail dom] [--session-id <id>]` prints the compact observe envelope on stdout. Input subcommands print a compact actuate envelope (`ok`, `frozen`, `retried`, `settled`, `foregrounded`). Pause/Break and MCP `stop` halt injection; CLI `stop` is a no-op without a live MCP lease. Confirm fence is not this crate's job yet.

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
