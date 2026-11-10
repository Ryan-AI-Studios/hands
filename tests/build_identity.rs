//! 0127 DoD-2: build identity. The CLI and the MCP `serverInfo.version` carry one canonical
//! string, `<CARGO_PKG_VERSION> (rev <12>|unknown, dirty=<0|1>)`, and the build-time stamp must
//! equal the live git tree at test time, so a stale stamp fails instead of passing vacuously.

// The same resolver that `build.rs` runs, so the fail-closed path is exercised directly.
#[path = "../build_support/identity.rs"]
mod identity;

use std::path::PathBuf;
use std::process::Command;

use rmcp::ServerHandler;

fn git_out(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}

fn cli_version_line() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_hands"))
        .arg("--version")
        .output()
        .expect("run hands --version");
    assert!(out.status.success(), "hands --version must exit 0");
    String::from_utf8(out.stdout)
        .expect("utf8 version output")
        .trim_end()
        .to_string()
}

fn is_canonical_stamp(rev: &str, dirty: &str) -> bool {
    let rev_ok = rev == "unknown"
        || (rev.len() == 12
            && rev
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    let dirty_ok = dirty == "0" || dirty == "1";
    // `unknown` is only ever paired with dirty=1 (fail closed).
    rev_ok && dirty_ok && (rev != "unknown" || dirty == "1")
}

#[test]
fn build_identity_string_format() {
    let line = cli_version_line();
    let prefix = format!("hands {} (rev ", env!("CARGO_PKG_VERSION"));
    assert!(
        !line.starts_with("hands hands"),
        "clap must not double the binary name: {line}"
    );
    let rest = line
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("expected `{prefix}…`, got `{line}`"));
    let rest = rest
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("missing closing paren: {line}"));
    let (rev, dirty) = rest
        .split_once(", dirty=")
        .unwrap_or_else(|| panic!("missing dirty field: {line}"));
    assert!(
        is_canonical_stamp(rev, dirty),
        "non-canonical identity `{rev}` / `{dirty}` in `{line}`"
    );
}

#[test]
fn build_identity_matches_live_tree() {
    // Non-vacuous: compares the compile-time stamp with git evaluated now, not with a regex.
    let stamped_rev = env!("HANDS_BUILD_REV");
    let stamped_dirty = env!("HANDS_BUILD_DIRTY");
    let live_rev = git_out(&["rev-parse", "--short=12", "HEAD"]);
    let live_status = git_out(&["status", "--porcelain"]);
    match (live_rev, live_status) {
        (Some(rev), Some(status)) => {
            assert_eq!(
                stamped_rev, rev,
                "stale build stamp: rebuild so HANDS_BUILD_REV follows HEAD"
            );
            let expect_dirty = if status.is_empty() { "0" } else { "1" };
            assert_eq!(
                stamped_dirty, expect_dirty,
                "stale build stamp: rebuild so HANDS_BUILD_DIRTY follows the worktree"
            );
        }
        _ => assert_eq!(
            (stamped_rev, stamped_dirty),
            ("unknown", "1"),
            "git is unavailable now, so the stamp must be the fail-closed label"
        ),
    }
}

#[test]
fn build_identity_fail_closed() {
    // A directory that is not a git work tree must resolve to `unknown` and dirty.
    let dir: PathBuf =
        std::env::temp_dir().join(format!("hands-identity-nonrepo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create non-repo dir");
    let id = identity::resolve(&dir);
    let _ = std::fs::remove_dir(&dir);
    assert_eq!(id.rev, "unknown");
    assert!(id.dirty, "fail-closed identity must be dirty");
}

#[test]
fn build_identity_watch_set_names_every_input() {
    let id = identity::resolve(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let watch: Vec<String> = id
        .watch
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    for needle in [
        "build.rs",
        "build_support",
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "tests",
        "HEAD",
        "packed-refs",
    ] {
        assert!(
            watch
                .iter()
                .any(|w| w == needle || w.ends_with(&format!("/{needle}"))),
            "rerun set is missing `{needle}`: {watch:?}"
        );
    }
}

#[test]
fn mcp_server_info_version_matches_cli() {
    let info = hands::mcp::HandsServer.get_info();
    assert_eq!(info.server_info.name, "hands");
    assert_eq!(
        info.server_info.version,
        hands::build_identity::BUILD_VERSION
    );
    assert_eq!(
        cli_version_line(),
        format!("hands {}", hands::build_identity::BUILD_VERSION),
        "CLI and MCP must carry the same canonical identity"
    );
    assert!(
        info.capabilities.tools.is_some(),
        "tools capability must be advertised"
    );
    assert_eq!(
        hands::mcp::HandsServer::tool_router().list_all().len(),
        24,
        "all 24 MCP tools must remain exposed"
    );
}
