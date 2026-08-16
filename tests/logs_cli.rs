//! Cross-process: process A appends under HANDS_LOGS_DIR; CLI `hands logs` reads it.

use std::process::Command;

#[test]
fn cli_logs_reads_prior_lines_and_does_not_mint() {
    let dir = std::env::temp_dir().join(format!(
        "hands-logs-cli-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let prev = std::env::var_os("HANDS_LOGS_DIR");
    unsafe { std::env::set_var("HANDS_LOGS_DIR", &dir) };

    let write = hands::logs::record(hands::logs::Event {
        schema: hands::logs::LOGS_SCHEMA.into(),
        ts: "2026-08-16T12:00:00".into(),
        session_id: "cli-a".into(),
        kind: "tool".into(),
        tool: Some("click".into()),
        ok: Some(true),
        error: None,
        target: None,
        fence: None,
        confirm: None,
        observe: None,
        type_meta: None,
        key: None,
        yield_info: None,
    });

    let exe = env!("CARGO_BIN_EXE_hands");
    let listed = Command::new(exe)
        .env("HANDS_LOGS_DIR", &dir)
        .args(["logs", "--session-id", "cli-a"])
        .output();
    let missing = Command::new(exe)
        .env("HANDS_LOGS_DIR", &dir)
        .args(["logs"])
        .output();
    let isolated = Command::new(exe)
        .env("HANDS_LOGS_DIR", &dir)
        .args(["logs", "--session-id", "cli-b"])
        .output();

    match prev {
        Some(v) => unsafe { std::env::set_var("HANDS_LOGS_DIR", v) },
        None => unsafe { std::env::remove_var("HANDS_LOGS_DIR") },
    }
    let _ = std::fs::remove_dir_all(&dir);

    write.expect("append in process A");
    let listed = listed.expect("spawn hands logs");
    assert!(
        listed.status.success(),
        "stderr {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("\"kind\":\"tool\""), "{stdout}");
    assert!(stdout.contains("cli-a"), "{stdout}");

    let missing = missing.expect("spawn hands logs missing id");
    assert!(!missing.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&missing.stderr),
        String::from_utf8_lossy(&missing.stdout)
    );
    assert!(
        err.contains("session-id") || err.contains("required") || err.contains("will not mint"),
        "{err}"
    );

    let isolated = isolated.expect("spawn hands logs other id");
    assert!(isolated.status.success());
    let other = String::from_utf8_lossy(&isolated.stdout);
    assert!(!other.contains("\"kind\":\"tool\""), "{other}");
}
