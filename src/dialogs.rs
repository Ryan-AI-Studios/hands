//! Promote visible dialogs / cookie walls / account choosers in the fused map.
//!
//! Pure detector: no COM, no HTTP, no ONNX. Compound phrases + ARIA dialog roles.

use std::collections::HashMap;

use crate::extract::{DialogHit, Element, Extract, take_chars};

pub const DIALOG_CAP: usize = 4;
pub const DIALOG_TEXT_MAX: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Account,
    Cookie,
    Dialog,
    Dismiss,
}

const ACCOUNT_PHRASES: &[&str] = &[
    "continue as",
    "sign in with google",
    "use another account",
    "choose an account",
];

const COOKIE_PHRASES: &[&str] = &[
    "accept all cookies",
    "accept cookies",
    "allow cookies",
    "reject all",
    "reject cookies",
    "cookie settings",
    "manage cookies",
    "we use cookies",
];

const DISMISS_PHRASES: &[&str] = &["not now", "no thanks"];
const DISMISS_WORDS: &[&str] = &["dismiss", "close"];

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Cookie => "cookie",
            Self::Dialog | Self::Dismiss => "dialog",
        }
    }
}

fn element_text(el: &Element) -> &str {
    el.text.as_deref().unwrap_or("")
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn contains_phrase(hay: &str, needle: &str) -> bool {
    lower(hay).contains(needle)
}

fn is_dialog_role(role: &str) -> bool {
    let r = role.trim();
    r.eq_ignore_ascii_case("dialog") || r.eq_ignore_ascii_case("alertdialog")
}

fn primary_kind(el: &Element) -> Option<Kind> {
    let text = element_text(el);
    if ACCOUNT_PHRASES.iter().any(|p| contains_phrase(text, p)) {
        return Some(Kind::Account);
    }
    if COOKIE_PHRASES.iter().any(|p| contains_phrase(text, p)) {
        return Some(Kind::Cookie);
    }
    if is_dialog_role(&el.role) && !text.trim().is_empty() {
        return Some(Kind::Dialog);
    }
    None
}

fn is_dismiss(el: &Element) -> bool {
    let text = element_text(el);
    if text.trim().is_empty() {
        return false;
    }
    if DISMISS_PHRASES.iter().any(|p| contains_phrase(text, p)) {
        return true;
    }
    let tokens: Vec<String> = lower(text)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    DISMISS_WORDS.iter().any(|w| tokens.iter().any(|t| t == w))
}

fn to_hit(el: &Element, kind: Kind) -> DialogHit {
    DialogHit {
        id: el.id.clone(),
        role: el.role.clone(),
        text: take_chars(element_text(el), DIALOG_TEXT_MAX),
        rect: el.rect,
        kind: kind.as_str().to_string(),
    }
}

/// Table-driven detector. Cap 4. Dedup by id. Dismiss siblings only after a
/// cookie or account hit. Prefer account, then cookie, then generic dialog,
/// then dismiss.
pub fn detect(elements: &[Element]) -> Vec<DialogHit> {
    let mut seen = std::collections::HashSet::new();
    let mut hits: Vec<(Kind, DialogHit)> = Vec::new();
    for el in elements {
        if seen.contains(&el.id) {
            continue;
        }
        if let Some(kind) = primary_kind(el) {
            seen.insert(el.id.clone());
            hits.push((kind, to_hit(el, kind)));
        }
    }
    let has_cookie_or_account = hits
        .iter()
        .any(|(k, _)| matches!(k, Kind::Account | Kind::Cookie));
    if has_cookie_or_account {
        for el in elements {
            if !seen.insert(el.id.clone()) {
                continue;
            }
            if is_dismiss(el) {
                hits.push((Kind::Dismiss, to_hit(el, Kind::Dismiss)));
            }
        }
    }
    hits.sort_by_key(|(k, _)| *k);
    hits.truncate(DIALOG_CAP);
    hits.into_iter().map(|(_, h)| h).collect()
}

/// Write `extract.dialogs` and stable-partition matching ids to the front of
/// `elements` (detect rank first, then original order of the rest).
pub fn promote(extract: &mut Extract, elements: &mut [Element]) {
    extract.dialogs = detect(elements);
    if extract.dialogs.is_empty() {
        return;
    }
    let rank: HashMap<String, usize> = extract
        .dialogs
        .iter()
        .enumerate()
        .map(|(i, hit)| (hit.id.clone(), i))
        .collect();
    elements.sort_by_key(|el| rank.get(&el.id).copied().unwrap_or(usize::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Rect;

    fn el(id: &str, role: &str, text: &str) -> Element {
        Element {
            id: id.into(),
            role: role.into(),
            text: Some(text.into()),
            rect: Rect {
                x: 0,
                y: 0,
                w: 40,
                h: 16,
            },
            grid: None,
        }
    }

    fn kinds(hits: &[DialogHit]) -> Vec<(&str, &str, &str)> {
        hits.iter()
            .map(|h| (h.id.as_str(), h.kind.as_str(), h.text.as_str()))
            .collect()
    }

    #[test]
    fn continue_as_is_account() {
        let hits = detect(&[
            el("uia:nav.1", "Button", "Search"),
            el("uia:42.2035806.4.0.0.10930", "Button", "Continue as Ryan"),
        ]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "uia:42.2035806.4.0.0.10930");
        assert_eq!(hits[0].kind, "account");
        assert!(hits[0].text.to_ascii_lowercase().contains("continue as"));
    }

    #[test]
    fn accept_cookies_is_cookie() {
        let a = detect(&[el("chr:8", "Button", "Accept cookies")]);
        assert_eq!(kinds(&a), vec![("chr:8", "cookie", "Accept cookies")]);
        let b = detect(&[el("chr:9", "Button", "Accept all cookies")]);
        assert_eq!(kinds(&b), vec![("chr:9", "cookie", "Accept all cookies")]);
    }

    #[test]
    fn alertdialog_named_is_dialog() {
        let hits = detect(&[el(
            "uia:1.9",
            "alertdialog",
            "Allow cars.com to use your location?",
        )]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "dialog");
        assert_eq!(hits[0].id, "uia:1.9");
    }

    #[test]
    fn negatives_do_not_match() {
        let els = [
            el("a", "Button", "Continue shopping"),
            el("b", "Button", "Accept offer"),
            el("c", "Button", "Sign in"),
            el("d", "Button", "Close"),
            el("e", "Button", "I'm not a robot"),
        ];
        assert!(detect(&els).is_empty());
    }

    #[test]
    fn dismiss_only_after_cookie_or_account() {
        let alone = detect(&[el("d", "Button", "Not now")]);
        assert!(alone.is_empty());
        let with = detect(&[
            el("c", "Button", "Accept cookies"),
            el("d", "Button", "Not now"),
        ]);
        assert_eq!(with.len(), 2);
        assert_eq!(with[0].kind, "cookie");
        assert_eq!(with[1].id, "d");
        assert_eq!(with[1].kind, "dialog");
    }

    #[test]
    fn cap_four_prefers_account_then_cookie() {
        let els = [
            el("nav", "Button", "Home"),
            el("c1", "Button", "Accept cookies"),
            el("c2", "Button", "Manage cookies"),
            el("a1", "Button", "Continue as Ryan"),
            el("d1", "Button", "Not now"),
            el("d2", "Button", "Close"),
            el("g", "Dialog", "Location permission"),
        ];
        let hits = detect(&els);
        assert!(hits.len() <= DIALOG_CAP);
        assert_eq!(hits[0].kind, "account");
        assert_eq!(hits[0].id, "a1");
        assert_eq!(hits[1].kind, "cookie");
        assert!(hits.iter().any(|h| h.id == "c1" || h.id == "c2"));
    }

    #[test]
    fn dedup_by_id() {
        let dup = el("same", "Button", "Continue as Ryan");
        let hits = detect(&[dup.clone(), dup]);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn text_capped_at_80() {
        let long = format!("Continue as {}", "R".repeat(200));
        let hits = detect(&[el("a", "Button", &long)]);
        assert_eq!(hits[0].text.chars().count(), DIALOG_TEXT_MAX);
    }

    #[test]
    fn promote_writes_and_fronts() {
        let mut extract = Extract {
            title: "T".into(),
            url: None,
            main_text: String::new(),
            cards: Vec::new(),
            dialogs: Vec::new(),
        };
        let mut elements = vec![
            el("chr:0", "Button", "Search"),
            el("uia:late", "Button", "Continue as Ryan"),
            el("chr:1", "Button", "Filters"),
        ];
        promote(&mut extract, &mut elements);
        assert_eq!(extract.dialogs.len(), 1);
        assert_eq!(extract.dialogs[0].id, "uia:late");
        assert_eq!(elements[0].id, "uia:late");
        assert_eq!(elements[1].id, "chr:0");
        assert_eq!(elements[2].id, "chr:1");
    }
}
