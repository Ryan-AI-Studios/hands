# Hands

Windows **eyes-and-hands** MCP/CLI for [Helping Hands](https://github.com/Ryan-AI-Studios/hands).

This directory is the **product git root** (Execution Repo). Planning, ADRs, and conductor tracks live one level up at `C:\dev\Helping-Hands\` and are **not** part of this repository.

## Clone

```text
https://github.com/Ryan-AI-Studios/hands
```

## Tools (this directory only)

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ledgerful doctor --json
# workRoot/stateDir must be this directory, not C:\dev\Helping-Hands
```

`ai-brains context` / `ledgerful init` already ran here. Re-run them only if `.env` / `.ledgerful` are missing. Never init in the planning root.

## Build / test

The `hands` crate lives in this directory (Windows-first; `rust-toolchain.toml` pins the toolchain).

```powershell
cd C:\dev\Helping-Hands\hands
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

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
```

`hands mcp` serves stdio MCP (`observe`, `click`, `hover`, `type`, `key`, `scroll`, `wait_settle`, `stop`, `confirm`). `hands observe [--detail dom] [--session-id <id>]` prints a compact observe envelope (screenshot **path**, 100px grid descriptor, UIA map, capped extract). Image bytes are never inlined.

This binary owns the confirm fence. `click` and `key enter`/`return` refuse irreversible/gray-zone controls unless a matching domain+category allow exists (`ok: false` plus a compact `fence` object; no cursor move). `type` containing a newline is a tool error — use `key enter` to submit. After a refuse, the harness must call `confirm` (`once` / `session` / `persist`) and retry. Grok is always-approve, so a TUI prompt is not the fence.

Input commands (`click` / `hover` / `type` / `key` / `scroll` / `wait-settle`) install a desk lease for the duration of the process: physical mouse/keyboard freezes injection; Pause/Break always aborts. Pause/Break and `stop` wipe session/once allows (desk-wide) and leave persist. `hands confirm` does **not** install the desk lease. `hands stop` as a one-shot CLI still clears session allows; injection itself is a documented no-op without a live MCP lease (use MCP `stop`, or Pause/Break during a live command).

## What this is

A harness (Grok Build, Codex, Claude Code, OpenCode) uses this process to **see** the desktop and **move** the real mouse/keyboard on daily Chrome — no Playwright/CDP.

Product intent lives in the planning tree: `C:\dev\Helping-Hands\SHARED-UNDERSTANDING.md`.
