# Ledgerful Installation

If Ledgerful is not installed or not on the system `PATH`, follow these instructions.
Canonical binary name: **`ledgerful`**. The Unix installer may also place a short alias **`ldg`**.

Public repo: **[Ryan-AI-Studios/Ledgerful](https://github.com/Ryan-AI-Studios/Ledgerful)**.

## One-line installers (release binaries)

### Linux / macOS (Bash)

```bash
curl -fsSL https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.sh | sh
```

### Windows (PowerShell)

```powershell
iwr https://raw.githubusercontent.com/Ryan-AI-Studios/Ledgerful/main/install/install.ps1 -UseBasicParsing | iex
```

After installation, open a new terminal (or refresh `PATH`) so `ledgerful` resolves.

## Package managers

```bash
# Homebrew
brew install Ryan-AI-Studios/tap/ledgerful

# Scoop
scoop bucket add ledgerful https://github.com/Ryan-AI-Studios/scoop-bucket
scoop install ledgerful

# cargo-binstall (prebuilt if available)
cargo binstall --git https://github.com/Ryan-AI-Studios/Ledgerful
```

Winget: package is live on winget (accepted 2026-07-30):
`winget install Ledgerful.Ledgerful`. Community package metadata may lag
engine releases (live engine is in the v0.2.4 area); prefer the install script,
Scoop/Homebrew, or `cargo binstall` when you need the absolute latest release.

## MCP wrapper (npm)

Installs the `@ledgerful/mcp-server` package, which downloads a pinned engine release binary:

```bash
npx @ledgerful/mcp-server
# or: npm i -g @ledgerful/mcp-server
```

Engine pin is `ledgerfulEngineTag` on the published package — it must match a real GitHub release.

## From source

```bash
cargo install --path .          # from a clone of Ryan-AI-Studios/Ledgerful
# or reinstall after local edits:
ledgerful update --binary
```

## Starter config and credentials

Init template precedence is:

1. an existing path named by `LEDGERFUL_DEFAULT_CONFIG`;
2. `~/.ledgerful/default-config.toml`;
3. Ledgerful's built-in template.

Before publishing a new repo config, `ledgerful init` removes secret-bearing
assignments and structured connection URLs containing credentials. It reports
only the removed key paths. Use `GEMINI_API_KEY`, `OLLAMA_CLOUD_API_KEY`, or
the legacy `OLLAMA_API_KEY` in the process environment or an ignored repo-local
`.env`; TOML `${VAR}` interpolation is not supported.

## Binary names and updates

- Prefer **`ledgerful`** in docs, scripts, and agent instructions.
- **`ldg`** is an optional short name created by the shell installer on some platforms.
- Windows installer currently installs `ledgerful.exe` only.
- If Windows reports the binary is locked during update, close processes using it and run:

```powershell
ledgerful update --binary
```

Ledgerful stages and verifies replacements in its install directory and does not search for or
delete similarly named binaries elsewhere on `PATH`.

## First command after install

```bash
ledgerful doctor
```
