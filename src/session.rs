const ENV_KEYS: [&str; 3] = [
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "GROK_SESSION_ID",
];

/// Resolve `session_id`: explicit (non-empty) → env sniff in order → mint UUID v4.
///
/// Missing or empty values fall through. There is no process-global last id.
pub fn resolve_session_id(
    explicit: Option<&str>,
    mut env: impl FnMut(&str) -> Option<String>,
) -> String {
    if let Some(value) = nonempty(explicit) {
        return value.to_string();
    }
    for key in ENV_KEYS {
        if let Some(value) = nonempty(env(key).as_deref()) {
            return value.to_string();
        }
    }
    uuid::Uuid::new_v4().to_string()
}

pub fn resolve_session_id_from_os(explicit: Option<&str>) -> String {
    resolve_session_id(explicit, |key| std::env::var(key).ok())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn explicit_wins_over_env() {
        let id = resolve_session_id(
            Some("explicit-1"),
            env_of(&[
                ("CODEX_THREAD_ID", "codex"),
                ("CLAUDE_CODE_SESSION_ID", "claude"),
                ("GROK_SESSION_ID", "grok"),
            ]),
        );
        assert_eq!(id, "explicit-1");
    }

    #[test]
    fn empty_explicit_falls_through() {
        let id = resolve_session_id(Some(""), env_of(&[("GROK_SESSION_ID", "grok")]));
        assert_eq!(id, "grok");
        let id = resolve_session_id(Some("   "), env_of(&[("GROK_SESSION_ID", "grok")]));
        assert_eq!(id, "grok");
    }

    #[test]
    fn codex_env_wins_over_later() {
        let id = resolve_session_id(
            None,
            env_of(&[
                ("CODEX_THREAD_ID", "codex"),
                ("CLAUDE_CODE_SESSION_ID", "claude"),
                ("GROK_SESSION_ID", "grok"),
            ]),
        );
        assert_eq!(id, "codex");
    }

    #[test]
    fn claude_env_used_when_codex_missing() {
        let id = resolve_session_id(
            None,
            env_of(&[
                ("CLAUDE_CODE_SESSION_ID", "claude"),
                ("GROK_SESSION_ID", "grok"),
            ]),
        );
        assert_eq!(id, "claude");
    }

    #[test]
    fn grok_env_used_when_earlier_missing() {
        let id = resolve_session_id(None, env_of(&[("GROK_SESSION_ID", "grok")]));
        assert_eq!(id, "grok");
    }

    #[test]
    fn empty_env_values_fall_through() {
        let id = resolve_session_id(
            None,
            env_of(&[
                ("CODEX_THREAD_ID", ""),
                ("CLAUDE_CODE_SESSION_ID", "   "),
                ("GROK_SESSION_ID", "grok"),
            ]),
        );
        assert_eq!(id, "grok");
    }

    #[test]
    fn mint_uniqueness() {
        let a = resolve_session_id(None, |_| None);
        let b = resolve_session_id(None, |_| None);
        assert_ne!(a, b);
        assert!(!a.is_empty());
        assert!(!b.is_empty());
    }
}
