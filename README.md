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
```

`hands mcp` serves stdio MCP (`observe`, `click`, `hover`, `type`, `key`, `scroll`, `wait_settle`, `stop`). `hands observe [--detail dom] [--session-id <id>]` prints a compact observe envelope (screenshot **path**, 100px grid descriptor, UIA map, capped extract). Image bytes are never inlined.

Input commands (`click` / `hover` / `type` / `key` / `scroll` / `wait-settle`) install a desk lease for the duration of the process: physical mouse/keyboard freezes injection; Pause/Break always aborts. `hands stop` as a one-shot CLI is a documented no-op (use MCP `stop`, or Pause/Break during a live command). There is no confirm fence in this binary yet.

## What this is

A harness (Grok Build, Codex, Claude Code, OpenCode) uses this process to **see** the desktop and **move** the real mouse/keyboard on daily Chrome — no Playwright/CDP.

Product intent lives in the planning tree: `C:\dev\Helping-Hands\SHARED-UNDERSTANDING.md`.
