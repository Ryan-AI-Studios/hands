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

**Never** commit `conductor/`, `docs/adr/`, `SHARED-UNDERSTANDING.md`, planner handoff, or `.agents/` into this repo.

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
cargo run -- confirm --help
cargo run -- attach --help
cargo run -- pick --help
cargo run -- ground --help
cargo run -- logs --help
cargo run -- native-host --help
cargo run -- native-host-manifest --help
```

`hands mcp` is the stdio MCP server (`observe` plus `click` / `hover` / `type` / `key` / `scroll` / `wait_settle` / `stop` / `confirm` / `attach` / `pick` / `ground` / `logs`). `hands observe [--detail dom] [--session-id <id>]` prints the compact observe envelope on stdout (UIA + Chrome `chr:` when connected; `chrome_connected` on the envelope). **`observe` does not launch Chrome.** **`observe` does not call Gemma.** Input subcommands print a compact actuate envelope (`ok`, `frozen`, `retried`, `settled`, `foregrounded`, optional `fence`). Pause/Break and MCP `stop` halt injection and wipe session/once allows; persist stays; **logs stay** (`%LOCALAPPDATA%\hands\logs\`, override `HANDS_LOGS_DIR`). `type` logs `len` only; observe logs path + counts, not `main_text` / url / DOM. `hands logs --session-id <id>` tails (does not mint); `--list` lists files. `desk` is reserved; `desk.jsonl` is unbounded (no rotation). CLI `stop` still clears session allows; injection is a no-op without a live MCP lease. The confirm fence lives in this binary: refuse gated `click` / `key enter` without an allow; after a refuse the harness must call `confirm` then retry (Grok is always-approve). `hands confirm`, `hands attach`, `hands pick`, `hands ground`, and `hands logs` do not install the desk lease.

`hands pick --query <q> [--elements-json <path>] [--observe-path <path>] [--session-id <id>]` and `hands ground --query <q> [--observe-path <path>] [--screenshot <path>] [--element-id <id>] [--x --y --w --h] [--session-id <id>]` are on-demand helpers to local Gemma at `http://127.0.0.1:8081` (`HANDS_GEMMA_URL`, loopback http only). `HANDS_GEMMA_TIMEOUT_MS` (default 90000, min 5000), `HANDS_GEMMA_FORCE_TEXT` (`1`/`true`/`yes`, case-insensitive) skips images, `HANDS_GEMMA_API_KEY` optional Bearer. **8081 down is a tool error**, never a compile/test gate. `pick` always sends a text element list. `ground` sends a PNG crop only when `/v1/models` reports multimodal; otherwise it degrades to text. mmproj install / live demo stays **0011**.

`hands attach [--plan] [--session-id <id>]` attaches to daily Chrome (`Chrome_WidgetWin_1` + `chrome.exe`) or launches `chrome.exe about:blank` with no `--` flags. `--plan` never spawns. `HANDS_CHROME_EXE` set + missing file is a hard error. Attach does not sideload and does not kill Chrome. Sideload/register is **0011**.

Chrome fusion: unpacked MV3 at `extension/` (id `fdnpjnnnmfhlpgaabjflhjoepmejcnha`), host name `com.helpinghands.host`, pipe `\\.\pipe\hands-chrome` (`HANDS_CHROME_PIPE`). `HANDS_CHROME_SNAPSHOT` is a host-double fixture (no Chrome). `chr:` is `chr:<u32>` only (no leading zeros except `chr:0`). Toolbar/DPR conversion is **approximate**; 0011 demo on the primary monitor. Sideload/register is **0011**. No CDP. `hands native-host` is Chrome-spawned (no desk lease); `hands native-host-manifest` prints filled JSON.

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
