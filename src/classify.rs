//! Pure confirm-fence classifier and domain grain helpers. No COM.

use std::fmt;
use std::str::FromStr;

use crate::error::HandsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Money,
    Messages,
    Applications,
    Account,
    Deletes,
    Installs,
    Elevated,
    Save,
    Social,
    Lead,
}

impl Category {
    pub const ALL: [Self; 10] = [
        Self::Money,
        Self::Messages,
        Self::Applications,
        Self::Account,
        Self::Deletes,
        Self::Installs,
        Self::Elevated,
        Self::Save,
        Self::Social,
        Self::Lead,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Money => "money",
            Self::Messages => "messages",
            Self::Applications => "applications",
            Self::Account => "account",
            Self::Deletes => "deletes",
            Self::Installs => "installs",
            Self::Elevated => "elevated",
            Self::Save => "save",
            Self::Social => "social",
            Self::Lead => "lead",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, HandsError> {
        raw.parse()
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Category {
    type Err = HandsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "money" => Ok(Self::Money),
            "messages" => Ok(Self::Messages),
            "applications" => Ok(Self::Applications),
            "account" => Ok(Self::Account),
            "deletes" => Ok(Self::Deletes),
            "installs" => Ok(Self::Installs),
            "elevated" => Ok(Self::Elevated),
            "save" => Ok(Self::Save),
            "social" => Ok(Self::Social),
            "lead" => Ok(Self::Lead),
            other => Err(HandsError::Fence(format!("unknown category '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub name: String,
    pub role: String,
    pub window_title: String,
    pub window_class: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Free,
    Gated { category: Category },
}

#[derive(Clone, Copy)]
enum MatchKind {
    Any,
    SubmitLike,
    SubmitOrExact,
}

const FREE_PHRASES: &[&str] = &[
    "accept all",
    "accept cookies",
    "allow all",
    "allow cookies",
    "accept",
    "allow",
    "not now",
    "no thanks",
    "dismiss",
    "skip",
    "maybe later",
    "update later",
    "apply filters",
    "enter zip",
    "use this location",
    "allow location",
    "zip",
    "location",
    "sign in later",
    "stay signed out",
    "no thanks sign in",
    "language",
    "english",
    "español",
    "espanol",
];

const GATED_PHRASES: &[(&str, Category, MatchKind)] = &[
    ("place order", Category::Money, MatchKind::Any),
    ("add payment", Category::Money, MatchKind::Any),
    ("checkout", Category::Money, MatchKind::Any),
    ("purchase", Category::Money, MatchKind::Any),
    ("donate", Category::Money, MatchKind::Any),
    ("buy", Category::Money, MatchKind::Any),
    ("pay", Category::Money, MatchKind::Any),
    ("send inmail", Category::Messages, MatchKind::Any),
    ("send message", Category::Messages, MatchKind::Any),
    ("inmail", Category::Messages, MatchKind::Any),
    ("send", Category::Messages, MatchKind::SubmitLike),
    ("reply", Category::Messages, MatchKind::SubmitLike),
    ("submit application", Category::Applications, MatchKind::Any),
    ("easy apply", Category::Applications, MatchKind::Any),
    ("apply now", Category::Applications, MatchKind::Any),
    ("apply", Category::Applications, MatchKind::SubmitLike),
    ("change password", Category::Account, MatchKind::Any),
    ("delete account", Category::Account, MatchKind::Any),
    ("update payment method", Category::Account, MatchKind::Any),
    ("transfer", Category::Account, MatchKind::Any),
    ("permanently delete", Category::Deletes, MatchKind::Any),
    ("remove listing", Category::Deletes, MatchKind::Any),
    ("empty trash", Category::Deletes, MatchKind::Any),
    ("delete", Category::Deletes, MatchKind::Any),
    ("install", Category::Installs, MatchKind::SubmitOrExact),
    ("setup", Category::Installs, MatchKind::SubmitOrExact),
    ("run", Category::Installs, MatchKind::SubmitOrExact),
    ("save changes", Category::Save, MatchKind::Any),
    ("save listing", Category::Save, MatchKind::Any),
    ("save", Category::Save, MatchKind::Any),
    ("follow", Category::Social, MatchKind::Any),
    ("connect", Category::Social, MatchKind::Any),
    ("invite", Category::Social, MatchKind::Any),
    ("check availability", Category::Lead, MatchKind::Any),
    ("request quote", Category::Lead, MatchKind::Any),
    ("contact dealer", Category::Lead, MatchKind::Any),
    ("submit", Category::Lead, MatchKind::SubmitLike),
];

pub fn classify(evidence: &Evidence) -> Verdict {
    if is_elevated(evidence) {
        return Verdict::Gated {
            category: Category::Elevated,
        };
    }
    let name = evidence.name.as_str();
    // Whole-name only: a free word inside "Save location" must not hide `save`.
    if FREE_PHRASES
        .iter()
        .any(|phrase| name_is_phrase(name, phrase))
    {
        return Verdict::Free;
    }
    let submit = is_submit_like(evidence);
    let exact_name = name.trim();
    let mut best: Option<(&str, Category)> = None;
    for &(phrase, category, kind) in GATED_PHRASES {
        if !contains_phrase(name, phrase) {
            continue;
        }
        if best.is_some_and(|(hit, _)| hit.len() >= phrase.len()) {
            continue;
        }
        let allowed = match kind {
            MatchKind::Any => true,
            MatchKind::SubmitLike => submit,
            MatchKind::SubmitOrExact => submit || exact_name.eq_ignore_ascii_case(phrase),
        };
        if allowed {
            best = Some((phrase, category));
        }
    }
    match best {
        Some((_, category)) => Verdict::Gated { category },
        None => Verdict::Free,
    }
}

fn is_elevated(evidence: &Evidence) -> bool {
    let blob = format!(
        "{} {} {}",
        evidence.name, evidence.window_title, evidence.window_class
    )
    .to_ascii_lowercase();
    blob.contains("user account control") || blob.contains("consent.exe")
}

fn is_submit_like(evidence: &Evidence) -> bool {
    // Role only — names like "Reply on LinkedIn" must not count as a link/button.
    let role = evidence.role.to_ascii_lowercase();
    role.contains("button")
        || role.contains("hyperlink")
        || role.contains("link")
        || role.contains("splitbutton")
        || role.contains("split button")
        || role.contains("menuitem")
        || role.contains("menu item")
}

fn name_is_phrase(name: &str, phrase: &str) -> bool {
    let name: String = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    name == phrase.to_ascii_lowercase()
}

fn contains_phrase(haystack: &str, phrase: &str) -> bool {
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let needle: Vec<char> = phrase.to_lowercase().chars().collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    for start in 0..=hay.len() - needle.len() {
        if hay[start..start + needle.len()] != needle[..] {
            continue;
        }
        let before_ok = start == 0 || !hay[start - 1].is_alphanumeric();
        let after = start + needle.len();
        let after_ok = after == hay.len() || !hay[after].is_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

pub fn normalize_host(raw: &str) -> String {
    let mut host = raw.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix("www.") {
        host = rest.to_string();
    }
    host
}

pub fn host_matches(allow: &str, host: &str) -> bool {
    let allow = normalize_host(allow);
    let host = normalize_host(host);
    if is_exact_grain(&allow) || is_exact_grain(&host) {
        return allow == host;
    }
    host == allow || host.ends_with(&format!(".{allow}"))
}

fn is_exact_grain(s: &str) -> bool {
    s == "desktop" || s == "unknown"
}

pub fn resolve_domain(
    last_url: Option<&str>,
    title: &str,
    address_value: &str,
    window_class: &str,
) -> String {
    if let Some(raw) = last_url
        && let Some(host) = host_from_text(raw)
    {
        return host;
    }
    if let Some(host) = host_from_text(title).or_else(|| host_from_text(address_value)) {
        return host;
    }
    if window_class.starts_with("Chrome_WidgetWin") {
        "unknown".into()
    } else {
        "desktop".into()
    }
}

fn host_from_text(raw: &str) -> Option<String> {
    if let Some(host) = host_from_url_prefix(raw) {
        return Some(host);
    }
    bare_host(raw)
}

fn host_from_url_prefix(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let (idx, scheme_len) = if let Some(i) = lower.find("https://") {
        (i, 8)
    } else {
        let i = lower.find("http://")?;
        (i, 7)
    };
    parse_host_after_scheme(&raw[idx + scheme_len..])
}

fn parse_host_after_scheme(s: &str) -> Option<String> {
    let end = s
        .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
        .unwrap_or(s.len());
    let mut host = s[..end].trim();
    if let Some(at) = host.rfind('@') {
        host = &host[at + 1..];
    }
    if let Some(colon) = host.rfind(':')
        && host[colon + 1..].bytes().all(|b| b.is_ascii_digit())
    {
        host = &host[..colon];
    }
    if host.is_empty() {
        return None;
    }
    Some(normalize_host(host))
}

fn bare_host(raw: &str) -> Option<String> {
    for token in raw.split(|c: char| c.is_whitespace() || matches!(c, '/' | ',' | ';' | '|')) {
        let token =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-');
        if looks_like_host(token) {
            return Some(normalize_host(token));
        }
    }
    None
}

fn looks_like_host(s: &str) -> bool {
    if s.is_empty() || s.starts_with('.') || s.ends_with('.') || !s.contains('.') {
        return false;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let tld = *parts.last().unwrap();
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && !p.starts_with('-')
            && !p.ends_with('-')
            && p.chars().any(|c| c.is_ascii_alphabetic())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str, role: &str) -> Evidence {
        Evidence {
            name: name.into(),
            role: role.into(),
            window_title: String::new(),
            window_class: String::new(),
        }
    }

    fn gated(name: &str, role: &str) -> Category {
        match classify(&ev(name, role)) {
            Verdict::Gated { category } => category,
            Verdict::Free => panic!("expected gated {name:?} / {role:?}"),
        }
    }

    fn free(name: &str, role: &str) {
        assert_eq!(
            classify(&ev(name, role)),
            Verdict::Free,
            "expected free {name:?} / {role:?}"
        );
    }

    #[test]
    fn category_display_and_parse_case_insensitive() {
        for cat in Category::ALL {
            assert_eq!(cat.to_string(), cat.as_str());
            assert_eq!(
                Category::parse(&cat.as_str().to_ascii_uppercase()).unwrap(),
                cat
            );
        }
        assert!(Category::parse("not-a-category").is_err());
    }

    #[test]
    fn shared_irreversible_money() {
        assert_eq!(gated("Buy", "button"), Category::Money);
        assert_eq!(gated("Checkout", "button"), Category::Money);
        assert_eq!(gated("Pay", "button"), Category::Money);
        assert_eq!(gated("Place order", "button"), Category::Money);
        assert_eq!(gated("Add payment", "button"), Category::Money);
        assert_eq!(gated("Donate", "button"), Category::Money);
        assert_eq!(gated("Purchase", "button"), Category::Money);
    }

    #[test]
    fn shared_irreversible_messages() {
        assert_eq!(gated("Send", "button"), Category::Messages);
        assert_eq!(gated("Send message", "button"), Category::Messages);
        assert_eq!(gated("InMail", "button"), Category::Messages);
        assert_eq!(gated("Send InMail", "button"), Category::Messages);
        assert_eq!(gated("Reply", "button"), Category::Messages);
        free("Send", "edit");
        free("Reply", "document");
    }

    #[test]
    fn shared_irreversible_applications() {
        assert_eq!(gated("Easy Apply", "button"), Category::Applications);
        assert_eq!(
            gated("Submit application", "button"),
            Category::Applications
        );
        assert_eq!(gated("Apply now", "hyperlink"), Category::Applications);
        assert_eq!(gated("Apply", "button"), Category::Applications);
        free("Apply", "edit");
    }

    #[test]
    fn shared_irreversible_account() {
        assert_eq!(gated("Change password", "button"), Category::Account);
        assert_eq!(gated("Delete account", "button"), Category::Account);
        assert_eq!(gated("Transfer", "button"), Category::Account);
        assert_eq!(gated("Update payment method", "button"), Category::Account);
    }

    #[test]
    fn shared_irreversible_deletes() {
        assert_eq!(gated("Delete", "button"), Category::Deletes);
        assert_eq!(gated("Permanently delete", "button"), Category::Deletes);
        assert_eq!(gated("Remove listing", "button"), Category::Deletes);
        assert_eq!(gated("Empty trash", "menuitem"), Category::Deletes);
    }

    #[test]
    fn shared_irreversible_installs() {
        assert_eq!(gated("Install", "button"), Category::Installs);
        assert_eq!(gated("Setup", "button"), Category::Installs);
        assert_eq!(gated("Run", "button"), Category::Installs);
        assert_eq!(gated("install", "text"), Category::Installs);
        free("Install now", "text");
        assert_eq!(gated("Install now", "button"), Category::Installs);
    }

    #[test]
    fn shared_irreversible_elevated() {
        let uac = Evidence {
            name: String::new(),
            role: "pane".into(),
            window_title: "User Account Control".into(),
            window_class: String::new(),
        };
        assert_eq!(
            classify(&uac),
            Verdict::Gated {
                category: Category::Elevated
            }
        );
        let consent = Evidence {
            name: String::new(),
            role: String::new(),
            window_title: String::new(),
            window_class: "consent.exe".into(),
        };
        assert_eq!(
            classify(&consent),
            Verdict::Gated {
                category: Category::Elevated
            }
        );
        assert_eq!(gated("User Account Control", "window"), Category::Elevated);
    }

    #[test]
    fn gray_zone_confirm_save_social_lead() {
        assert_eq!(gated("Save", "button"), Category::Save);
        assert_eq!(gated("Save changes", "button"), Category::Save);
        assert_eq!(gated("Save listing", "button"), Category::Save);
        assert_eq!(gated("Follow", "button"), Category::Social);
        assert_eq!(gated("Connect", "button"), Category::Social);
        assert_eq!(gated("Invite", "button"), Category::Social);
        assert_eq!(gated("Check availability", "button"), Category::Lead);
        assert_eq!(gated("Request quote", "button"), Category::Lead);
        assert_eq!(gated("Contact dealer", "button"), Category::Lead);
        assert_eq!(gated("Submit", "button"), Category::Lead);
        free("Submit", "edit");
    }

    #[test]
    fn gray_zone_free_lexicon() {
        free("Accept", "button");
        free("Accept all", "button");
        free("Accept cookies", "button");
        free("Allow all", "button");
        free("Allow cookies", "button");
        free("Allow", "button");
        free("Not now", "button");
        free("No thanks", "button");
        free("Dismiss", "button");
        free("Skip", "button");
        free("Maybe later", "button");
        free("Update later", "button");
        free("Apply filters", "button");
        free("ZIP", "button");
        free("Enter ZIP", "button");
        free("Use this location", "button");
        free("Allow location", "button");
        free("Location", "button");
        free("Sign in later", "button");
        free("Stay signed out", "button");
        free("No thanks sign in", "button");
        free("Language", "button");
        free("English", "button");
        free("Español", "button");
    }

    #[test]
    fn apply_filters_free_easy_apply_gated() {
        free("Apply filters", "button");
        assert_eq!(gated("Easy Apply", "button"), Category::Applications);
    }

    #[test]
    fn free_lexicon_is_whole_name_not_a_hidden_word() {
        assert_eq!(gated("Save location", "button"), Category::Save);
        assert_eq!(gated("Delete location", "button"), Category::Deletes);
        free("Reply on LinkedIn", "document");
        free("Send to LinkedIn", "edit");
        // After UIA pairs name with the named ancestor's role, Submit+button is gated.
        assert_eq!(gated("Submit", "button"), Category::Lead);
        free("Submit", "text");
    }

    #[test]
    fn unmatched_and_unlabeled_are_free() {
        free("Document", "document");
        free("", "button");
        free("Continue", "button");
        free("buyer", "button");
    }

    #[test]
    fn longest_match_wins() {
        assert_eq!(gated("Delete account", "button"), Category::Account);
        assert_eq!(gated("Send InMail", "button"), Category::Messages);
        assert_eq!(
            gated("Submit application", "button"),
            Category::Applications
        );
    }

    #[test]
    fn classify_does_not_take_caller_category() {
        let _ = classify(&ev("Easy Apply", "button"));
    }

    #[test]
    fn normalize_and_host_matches() {
        assert_eq!(normalize_host("WWW.LinkedIn.com"), "linkedin.com");
        assert!(host_matches("linkedin.com", "www.linkedin.com"));
        assert!(host_matches("linkedin.com", "jobs.linkedin.com"));
        assert!(!host_matches("jobs.linkedin.com", "www.linkedin.com"));
        assert!(!host_matches("jobs.linkedin.com", "linkedin.com"));
        assert!(host_matches("desktop", "desktop"));
        assert!(host_matches("unknown", "unknown"));
        assert!(!host_matches("desktop", "unknown"));
        assert!(!host_matches("unknown", "desktop"));
        assert!(!host_matches("desktop", "foo.desktop"));
        assert!(!host_matches("unknown", "foo.unknown"));
    }

    #[test]
    fn resolve_domain_order_and_fallbacks() {
        assert_eq!(
            resolve_domain(Some("https://www.linkedin.com/jobs"), "", "", ""),
            "linkedin.com"
        );
        assert_eq!(
            resolve_domain(None, "Inbox - https://jobs.linkedin.com/x", "", ""),
            "jobs.linkedin.com"
        );
        assert_eq!(
            resolve_domain(
                None,
                "Mail",
                "https://user:pass@mail.example.com:443/path",
                ""
            ),
            "mail.example.com"
        );
        assert_eq!(
            resolve_domain(None, "LinkedIn", "jobs.linkedin.com/view", ""),
            "jobs.linkedin.com"
        );
        assert_eq!(
            resolve_domain(None, "Google Chrome", "", "Chrome_WidgetWin_1"),
            "unknown"
        );
        assert_eq!(
            resolve_domain(None, "Untitled - Notepad", "", "Notepad"),
            "desktop"
        );
    }
}
