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
ai-brains context          # once
ledgerful init             # once
ai-brains preflight --summary
ledgerful doctor --json
```

## Build / test

Crate not scaffolded yet. After it exists:

```powershell
cd C:\dev\Helping-Hands\hands
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## What this is

A harness (Grok Build, Codex, Claude Code, OpenCode) uses this process to **see** the desktop and **move** the real mouse/keyboard on daily Chrome — no Playwright/CDP.

Product intent lives in the planning tree: `C:\dev\Helping-Hands\SHARED-UNDERSTANDING.md`.
