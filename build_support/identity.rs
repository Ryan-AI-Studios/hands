//! Build identity resolver (0127 DoD-2), shared by `build.rs` and `tests/build_identity.rs`.
//!
//! The label is `<rev>` (12 hex from `git rev-parse --short=12 HEAD`) plus a dirty flag from
//! `git status --porcelain`. Resolution fails closed: any git failure yields `rev unknown,
//! dirty=1`. The sha256 of the exe stays the authoritative identity (D-1); the label is never
//! evidence that a build is deployed.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub const UNKNOWN_REV: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// 12 lowercase hex digits, or `unknown`.
    pub rev: String,
    /// True when the worktree has changes, or when the identity could not be resolved.
    pub dirty: bool,
    /// Paths for `cargo:rerun-if-changed`. A change to any of them can alter the label.
    pub watch: Vec<PathBuf>,
}

impl Identity {
    pub fn dirty_flag(&self) -> &'static str {
        if self.dirty { "1" } else { "0" }
    }

    fn unknown(watch: Vec<PathBuf>) -> Self {
        Self {
            rev: UNKNOWN_REV.to_string(),
            dirty: true,
            watch,
        }
    }
}

/// Resolve the identity of the work tree at `repo`. Never panics and never returns a clean
/// label without a successful `git rev-parse` and `git status`.
pub fn resolve(repo: &Path) -> Identity {
    // Inputs that do not come from git. `tests` is included because the dirty flag covers the
    // test sources too, and the identity test compares the label against them. `build_support`
    // holds this resolver, which build.rs compiles in via #[path], so editing it must re-run.
    let mut watch: Vec<PathBuf> = [
        "build.rs",
        "build_support",
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "tests",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout)
            .ok()
            .map(|s| s.trim().to_string())
    };

    // The ref that HEAD names (`refs/heads/<branch>`), or `HEAD` when detached.
    let Some(refname) = git(&["rev-parse", "--symbolic-full-name", "HEAD"]) else {
        return Identity::unknown(watch);
    };
    // The files whose change means a new commit: the ref file, packed-refs and HEAD itself
    // (a branch switch rewrites HEAD). `--git-path` is worktree-safe.
    for what in [refname.as_str(), "packed-refs", "HEAD"] {
        match git(&["rev-parse", "--git-path", what]) {
            Some(p) if !p.is_empty() => watch.push(PathBuf::from(p)),
            _ => return Identity::unknown(watch),
        }
    }

    let Some(rev) = git(&["rev-parse", "--short=12", "HEAD"]) else {
        return Identity::unknown(watch);
    };
    let rev_ok = rev.len() == 12
        && rev
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !rev_ok {
        return Identity::unknown(watch);
    }

    let Some(status) = git(&["status", "--porcelain"]) else {
        return Identity::unknown(watch);
    };

    Identity {
        rev,
        dirty: !status.is_empty(),
        watch,
    }
}
