//! Canonical build identity for the CLI and the MCP surface (0127 DoD-2).
//!
//! `HANDS_BUILD_REV` and `HANDS_BUILD_DIRTY` are stamped by `build.rs`. The label is a
//! convenience: the sha256 of the exe stays the authoritative identity (D-1), and a
//! `dirty=1` or `rev unknown` build is never reported as deployed.

/// `<CARGO_PKG_VERSION> (rev <12>|unknown, dirty=<0|1>)`, with no binary-name prefix.
/// clap prints `hands <this>` for `--version`; MCP `serverInfo.version` carries it unchanged.
pub const BUILD_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (rev ",
    env!("HANDS_BUILD_REV"),
    ", dirty=",
    env!("HANDS_BUILD_DIRTY"),
    ")"
);

/// MCP `serverInfo.name`.
pub const SERVER_NAME: &str = "hands";
