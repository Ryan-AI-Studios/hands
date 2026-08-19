use serde::{Deserialize, Serialize};

use crate::space::Rect;

pub const TITLE_MAX_CHARS: usize = 200;
pub const MAIN_TEXT_MAX_CHARS: usize = 1500;
pub const DEFAULT_ELEMENT_CAP: usize = 250;
pub const DOM_ELEMENT_CAP: usize = 2000;
pub const VIEWPORT_ENVELOPE_ELEMENT_CAP: usize = 20;
pub const CARD_MILES_CAP: usize = 16;
pub const CARD_DEALER_CAP: usize = 48;
pub const CARD_DISTANCE_CAP: usize = 40;
pub const CARD_OF_CAP: usize = 12;
pub const RESULT_COUNT_CAP: usize = 24;
pub const LOCAL_MATCHES_CAP: usize = 24;
pub const EMPTY_STATE_CAP: usize = 120;
pub const ZIP_CAP: usize = 10;
pub const RADIUS_CAP: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Default,
    Dom,
}

impl Detail {
    pub fn element_cap(self) -> usize {
        match self {
            Self::Default => DEFAULT_ELEMENT_CAP,
            Self::Dom => DOM_ELEMENT_CAP,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Dom => "dom",
        }
    }

    pub fn parse_arg(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Self::Default),
            Some(s) if s.eq_ignore_ascii_case("default") => Ok(Self::Default),
            Some(s) if s.eq_ignore_ascii_case("dom") => Ok(Self::Dom),
            Some(other) => Err(format!(
                "unknown detail '{other}' (expected default or dom)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Button,
    Edit,
    Document,
    Hyperlink,
    ComboBox,
    ListItem,
    MenuItem,
    CheckBox,
    RadioButton,
    TabItem,
    TreeItem,
    Slider,
    SplitButton,
    Text,
    Other,
}

impl ControlKind {
    pub fn from_uia_id(id: i32) -> Self {
        match id {
            50_000 => Self::Button,
            50_002 => Self::CheckBox,
            50_003 => Self::ComboBox,
            50_004 => Self::Edit,
            50_005 => Self::Hyperlink,
            50_007 => Self::ListItem,
            50_011 => Self::MenuItem,
            50_013 => Self::RadioButton,
            50_015 => Self::Slider,
            50_019 => Self::TabItem,
            50_020 => Self::Text,
            50_024 => Self::TreeItem,
            50_030 => Self::Document,
            50_031 => Self::SplitButton,
            _ => Self::Other,
        }
    }

    pub fn is_hittable(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::Edit
                | Self::Document
                | Self::Hyperlink
                | Self::ComboBox
                | Self::ListItem
                | Self::MenuItem
                | Self::CheckBox
                | Self::RadioButton
                | Self::TabItem
                | Self::TreeItem
                | Self::Slider
                | Self::SplitButton
        )
    }

    pub fn contributes_main_text(self) -> bool {
        matches!(self, Self::Document | Self::Edit | Self::Text)
    }

    pub fn type_name(self) -> &'static str {
        match self {
            Self::Button => "Button",
            Self::Edit => "Edit",
            Self::Document => "Document",
            Self::Hyperlink => "Hyperlink",
            Self::ComboBox => "ComboBox",
            Self::ListItem => "ListItem",
            Self::MenuItem => "MenuItem",
            Self::CheckBox => "CheckBox",
            Self::RadioButton => "RadioButton",
            Self::TabItem => "TabItem",
            Self::TreeItem => "TreeItem",
            Self::Slider => "Slider",
            Self::SplitButton => "SplitButton",
            Self::Text => "Text",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RawNode {
    pub runtime_id: Vec<i32>,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub is_password: bool,
    pub rect: Rect,
    pub control_kind: ControlKind,
    pub is_control: bool,
    pub is_offscreen: bool,
    pub is_keyboard_focusable: bool,
}

impl RawNode {
    pub fn element_id(&self) -> Option<String> {
        if self.runtime_id.is_empty() {
            return None;
        }
        Some(format!(
            "uia:{}",
            self.runtime_id
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        ))
    }

    pub fn passes_filter(&self, detail: Detail) -> bool {
        if self.element_id().is_none() {
            return false;
        }
        if !self.is_control || self.is_offscreen || self.rect.w <= 0 || self.rect.h <= 0 {
            return false;
        }
        match detail {
            Detail::Dom => true,
            Detail::Default => self.is_keyboard_focusable || self.control_kind.is_hittable(),
        }
    }

    pub fn to_element(&self) -> Option<Element> {
        Some(Element {
            id: self.element_id()?,
            role: self.role.clone(),
            text: if self.is_password {
                None
            } else {
                Some(self.name.clone())
            },
            rect: self.rect,
            grid: None,
        })
    }

    pub fn main_text_piece(&self) -> Option<String> {
        if self.is_password || !self.control_kind.contributes_main_text() {
            return None;
        }
        let mut out = String::new();
        if !self.name.is_empty() {
            out.push_str(&self.name);
        }
        if let Some(value) = &self.value
            && !value.is_empty()
            && value != &self.name
        {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(value);
        }
        if out.is_empty() { None } else { Some(out) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    pub id: String,
    pub role: String,
    pub text: Option<String>,
    pub rect: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub title: String,
    pub price: String,
    pub href: String,
    pub rect: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miles: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dealer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<String>,
    #[serde(rename = "of", default, skip_serializing_if = "Option::is_none")]
    pub listing_of: Option<String>,
}

impl Default for Card {
    fn default() -> Self {
        Self {
            title: String::new(),
            price: String::new(),
            href: String::new(),
            rect: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            miles: None,
            dealer: None,
            distance: None,
            listing_of: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogHit {
    pub id: String,
    pub role: String,
    pub text: String,
    pub rect: Rect,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Extract {
    pub title: String,
    pub url: Option<String>,
    pub main_text: String,
    pub cards: Vec<Card>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dialogs: Vec<DialogHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_count: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_matches: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingMeta {
    pub result_count: Option<String>,
    pub local_matches: Option<String>,
    pub empty_state: Option<String>,
    pub zip: Option<String>,
    pub radius: Option<String>,
}

pub fn filter_nodes(nodes: &[RawNode], detail: Detail) -> (Vec<Element>, usize) {
    let cap = detail.element_cap();
    let mut elements = Vec::new();
    let mut matched = 0usize;
    for node in nodes {
        if !node.passes_filter(detail) {
            continue;
        }
        matched += 1;
        if elements.len() < cap
            && let Some(element) = node.to_element()
        {
            elements.push(element);
        }
    }
    (elements, matched)
}

pub fn extract_from_nodes(title: &str, nodes: &[RawNode]) -> Extract {
    let mut extract = Extract {
        title: take_chars(title, TITLE_MAX_CHARS),
        url: None,
        main_text: join_main_text(nodes),
        cards: Vec::new(),
        dialogs: Vec::new(),
        ..Default::default()
    };
    enrich_listing(&mut extract);
    extract
}

pub fn join_main_text(nodes: &[RawNode]) -> String {
    let mut main_text = String::new();
    for node in nodes {
        if let Some(piece) = node.main_text_piece() {
            if !main_text.is_empty() {
                main_text.push('\n');
            }
            main_text.push_str(&piece);
            if main_text.chars().count() >= MAIN_TEXT_MAX_CHARS {
                break;
            }
        }
    }
    take_chars(&main_text, MAIN_TEXT_MAX_CHARS)
}

pub fn http_https_url(raw: Option<&str>) -> Option<String> {
    let t = raw?.trim();
    if t.is_empty() {
        return None;
    }
    let https = t.len() >= 8 && t[..8].eq_ignore_ascii_case("https://");
    let http = t.len() >= 7 && t[..7].eq_ignore_ascii_case("http://");
    if https || http {
        Some(t.to_string())
    } else {
        None
    }
}

pub fn extract_fused(
    uia_title: &str,
    uia_nodes: &[RawNode],
    chrome_url: Option<&str>,
    chrome_title: &str,
    chrome_main: &str,
    cards: Vec<Card>,
    listing: ListingMeta,
) -> Extract {
    let title = if chrome_title.trim().is_empty() {
        uia_title
    } else {
        chrome_title
    };
    let main_text = if chrome_main.trim().is_empty() {
        join_main_text(uia_nodes)
    } else {
        take_chars(chrome_main, MAIN_TEXT_MAX_CHARS)
    };
    let mut extract = Extract {
        title: take_chars(title, TITLE_MAX_CHARS),
        url: http_https_url(chrome_url),
        main_text,
        cards,
        dialogs: Vec::new(),
        result_count: take_opt_chars(listing.result_count, RESULT_COUNT_CAP),
        local_matches: take_opt_chars(listing.local_matches, LOCAL_MATCHES_CAP),
        empty_state: take_opt_chars(listing.empty_state, EMPTY_STATE_CAP),
        zip: take_opt_chars(listing.zip, ZIP_CAP),
        radius: take_opt_chars(listing.radius, RADIUS_CAP),
    };
    enrich_listing(&mut extract);
    extract
}

pub fn take_chars(input: &str, max: usize) -> String {
    input.chars().take(max).collect()
}

pub fn take_opt_chars(raw: Option<String>, max: usize) -> Option<String> {
    let t = raw?.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(take_chars(&t, max))
    }
}

pub fn parse_miles(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some((start, num_end, digits, has_comma)) = next_number(text, from) {
        let after = skip_ws_bytes(text, num_end);
        if let Some(unit_len) = match_mi_unit(&text[after..]) {
            let after_unit = skip_ws_bytes(text, after + unit_len);
            if is_word_at(&text[after_unit..], "away") {
                from = num_end;
                continue;
            }
            if has_comma || digits >= 4 {
                let phrase = text[start..after + unit_len].trim();
                if !phrase.is_empty() {
                    return Some(take_chars(phrase, CARD_MILES_CAP));
                }
            }
        }
        from = num_end;
    }
    None
}

pub fn parse_distance(text: &str) -> Option<String> {
    if let Some(away) = parse_mi_away(text) {
        return Some(away);
    }
    parse_shipping_from(text)
}

pub fn parse_listing_of(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some((start, num_end, _digits, has_comma)) = next_number(text, from) {
        if has_comma {
            from = num_end;
            continue;
        }
        let after = skip_ws_bytes(text, num_end);
        if is_word_at(&text[after..], "of") {
            let after_of = skip_ws_bytes(text, after + 2);
            if let Some((m_start, m_end, _md, m_comma)) = next_number(text, after_of)
                && m_start == after_of
                && !m_comma
            {
                let n = &text[start..num_end];
                let m = &text[m_start..m_end];
                return Some(take_chars(&format!("{n} of {m}"), CARD_OF_CAP));
            }
        }
        from = num_end;
    }
    None
}

pub fn parse_result_count(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some((start, num_end, digits, _has_comma)) = next_number(text, from) {
        if digits == 0 {
            from = num_end;
            continue;
        }
        let mut unit_at = num_end;
        let plus = text.get(unit_at..).is_some_and(|s| s.starts_with('+'));
        if plus {
            unit_at += 1;
        }
        let after = skip_ws_bytes(text, unit_at);
        let rest = &text[after..];
        let unit = if is_word_at(rest, "matches") {
            Some("matches")
        } else if is_word_at(rest, "cars") {
            Some("cars")
        } else if is_word_at(rest, "results") {
            Some("results")
        } else {
            None
        };
        if let Some(unit) = unit {
            let num = &text[start..num_end];
            let phrase = if plus {
                format!("{num}+ {unit}")
            } else {
                format!("{num} {unit}")
            };
            return Some(take_chars(&phrase, RESULT_COUNT_CAP));
        }
        from = num_end;
    }
    None
}

pub fn parse_local_matches(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some((start, num_end, digits, has_comma)) = next_number(text, from) {
        if has_comma || digits == 0 {
            from = num_end;
            continue;
        }
        let after = skip_ws_bytes(text, num_end);
        if is_word_at(&text[after..], "local") {
            let n = &text[start..num_end];
            return Some(take_chars(&format!("{n} local"), LOCAL_MATCHES_CAP));
        }
        from = num_end;
    }
    None
}

pub fn parse_empty_state(text: &str) -> Option<String> {
    const PHRASES: &[&str] = &[
        "nothing fits those filters",
        "we couldn't find",
        "we couldnt find",
        "we couldn\u{2019}t find",
        "0 matches",
        "no cars match",
        "no results",
        "try a larger radius",
        "expand your search",
    ];
    let lower = text.to_ascii_lowercase();
    let mut hit: Option<(usize, usize)> = None;
    for phrase in PHRASES {
        if let Some(idx) = find_empty_phrase(&lower, phrase) {
            match hit {
                Some((start, _)) if idx >= start => {}
                _ => hit = Some((idx, idx + phrase.len())),
            }
        }
    }
    let (start, end) = hit?;
    let sent_start = text[..start]
        .rfind(['.', '!', '?', '\n', '\r'])
        .map(|i| i + 1)
        .unwrap_or(0);
    let sent_end = text[end..]
        .find(['.', '!', '?', '\n', '\r'])
        .map(|i| end + i + 1)
        .unwrap_or(text.len());
    let sentence = text.get(sent_start..sent_end)?.trim();
    if sentence.is_empty() {
        None
    } else {
        Some(take_chars(sentence, EMPTY_STATE_CAP))
    }
}

pub fn parse_zip_radius(url: &str, text: &str) -> (Option<String>, Option<String>) {
    let mut zip = query_value(url, "zip").map(|z| take_chars(z, ZIP_CAP));
    let mut radius = query_value(url, "maximum_distance")
        .map(|d| take_chars(&format!("{} mi", d.trim()), RADIUS_CAP));
    let (text_zip, text_radius) = parse_within_zip_radius(text);
    if zip.as_ref().is_none_or(|s| s.is_empty()) {
        zip = text_zip;
    }
    if radius.as_ref().is_none_or(|s| s.is_empty()) {
        radius = text_radius;
    }
    (
        zip.filter(|s| !s.is_empty()),
        radius.filter(|s| !s.is_empty()),
    )
}

pub fn enrich_listing(extract: &mut Extract) {
    let text = extract.main_text.as_str();
    let url = extract.url.as_deref().unwrap_or("");
    if extract.result_count.is_none() {
        extract.result_count = parse_result_count(text);
    }
    if extract.local_matches.is_none()
        && let Some(local) = parse_local_matches(text)
        && extract.result_count.as_deref() != Some(local.as_str())
    {
        extract.local_matches = Some(local);
    }
    if extract.empty_state.is_none() {
        extract.empty_state = parse_empty_state(text);
    }
    let (zip, radius) = parse_zip_radius(url, text);
    if extract.zip.is_none() {
        extract.zip = zip;
    }
    if extract.radius.is_none() {
        extract.radius = radius;
    }
    for card in &mut extract.cards {
        if card.miles.is_none() {
            card.miles = parse_miles(&card.title);
        }
        if card.distance.is_none() {
            card.distance = parse_distance(&card.title);
        }
        if card.listing_of.is_none() {
            card.listing_of = parse_listing_of(&card.title);
        }
    }
}

/// Dealer from a **card-scoped** blob only (already-collected card innerText).
/// Strip title / price / miles / distance / `of`; leftover is the dealer candidate.
/// Never call this on page `main_text` — that would guess from the footer.
pub fn parse_dealer(card_text: &str, title: &str, price: &str) -> Option<String> {
    let mut rest = card_text.to_string();
    if !title.trim().is_empty() {
        rest = rest.replace(title.trim(), " ");
    }
    if !price.trim().is_empty() {
        rest = rest.replace(price.trim(), " ");
    }
    if let Some(miles) = parse_miles(&rest) {
        rest = rest.replace(&miles, " ");
    }
    if let Some(distance) = parse_distance(&rest) {
        rest = rest.replace(&distance, " ");
    }
    if let Some(of) = parse_listing_of(&rest) {
        rest = rest.replace(&of, " ");
    }
    rest = strip_price_tokens(&rest);
    let rest = collapse_ws(&rest);
    if rest.chars().count() < 2 || !rest.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "used" | "new" | "save" | "view" | "details" | "more"
    ) {
        return None;
    }
    Some(take_chars(&rest, CARD_DEALER_CAP))
}

fn collapse_ws(text: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn strip_price_tokens(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '$' || c == '€' || c == '£' {
            let rest = &text[i + c.len_utf8()..];
            let digits = rest
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == ',' || *ch == '.')
                .count();
            if digits > 0 {
                for _ in 0..digits {
                    chars.next();
                }
                out.push(' ');
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn find_empty_phrase(lower: &str, phrase: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut search = 0;
    while search <= lower.len() {
        let rel = lower[search..].find(phrase)?;
        let idx = search + rel;
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_digit();
        let after = idx + phrase.len();
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return Some(idx);
        }
        search = idx + 1;
    }
    None
}

fn next_number(text: &str, from: usize) -> Option<(usize, usize, usize, bool)> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut digits = 0usize;
            let mut has_comma = false;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b',') {
                if bytes[i] == b',' {
                    has_comma = true;
                } else {
                    digits += 1;
                }
                i += 1;
            }
            return Some((start, i, digits, has_comma));
        }
        i += 1;
    }
    None
}

fn skip_ws_bytes(text: &str, from: usize) -> usize {
    if from >= text.len() {
        return text.len();
    }
    let rest = &text[from..];
    from + (rest.len() - rest.trim_start().len())
}

fn ascii_prefix_rest<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let mut sc = s.chars();
    for p in prefix.chars() {
        match sc.next() {
            Some(c) if c.eq_ignore_ascii_case(&p) => {}
            _ => return None,
        }
    }
    Some(&s[prefix.len()..])
}

fn is_word_end(rest: &str) -> bool {
    !rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

fn is_word_at(s: &str, word: &str) -> bool {
    ascii_prefix_rest(s, word).is_some_and(is_word_end)
}

fn match_mi_unit(s: &str) -> Option<usize> {
    if let Some(rest) = ascii_prefix_rest(s, "miles")
        && is_word_end(rest)
    {
        return Some(5);
    }
    if let Some(rest) = ascii_prefix_rest(s, "mi")
        && is_word_end(rest)
    {
        return Some(2);
    }
    None
}

fn parse_mi_away(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some((start, num_end, _digits, _has_comma)) = next_number(text, from) {
        let after = skip_ws_bytes(text, num_end);
        if let Some(unit_len) = match_mi_unit(&text[after..]) {
            let after_unit = skip_ws_bytes(text, after + unit_len);
            if is_word_at(&text[after_unit..], "away") {
                let phrase = text[start..after_unit + 4].trim();
                if !phrase.is_empty() {
                    return Some(take_chars(phrase, CARD_DISTANCE_CAP));
                }
            }
        }
        from = num_end;
    }
    None
}

fn parse_shipping_from(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let idx = lower.find("shipping from")?;
    let line = text[idx..]
        .split(['\n', '\r'])
        .next()
        .unwrap_or(&text[idx..])
        .trim();
    if line.is_empty() {
        None
    } else {
        Some(take_chars(line, CARD_DISTANCE_CAP))
    }
}

fn query_value<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let q = url.split_once('?')?.1;
    let q = q.split('#').next().unwrap_or(q);
    for pair in q.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k.eq_ignore_ascii_case(key) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn parse_within_zip_radius(text: &str) -> (Option<String>, Option<String>) {
    let lower = text.to_ascii_lowercase();
    let mut search = 0;
    while let Some(rel) = lower[search..].find("within ") {
        let at = search + rel + "within ".len();
        if let Some((n_start, n_end, _digits, _comma)) = next_number(text, at) {
            let num_at = skip_ws_bytes(text, at);
            if n_start == num_at {
                let after = skip_ws_bytes(text, n_end);
                if let Some(unit_len) = match_mi_unit(&text[after..]) {
                    let after_unit = skip_ws_bytes(text, after + unit_len);
                    if is_word_at(&text[after_unit..], "of") {
                        let after_of = skip_ws_bytes(text, after_unit + 2);
                        if let Some(zip) = take_zip_at(text, after_of) {
                            let radius =
                                take_chars(&format!("{} mi", &text[n_start..n_end]), RADIUS_CAP);
                            return (Some(zip), Some(radius));
                        }
                    }
                }
            }
        }
        search += rel + 1;
    }
    (None, None)
}

fn take_zip_at(text: &str, from: usize) -> Option<String> {
    if from >= text.len() {
        return None;
    }
    let rest = &text[from..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 5 {
        return None;
    }
    let after = &rest[5..];
    if let Some(plus) = after.strip_prefix('-') {
        let plus4: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
        if plus4.len() == 4 {
            return Some(take_chars(&format!("{digits}-{plus4}"), ZIP_CAP));
        }
    }
    Some(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: ControlKind, name: &str) -> RawNode {
        RawNode {
            runtime_id: vec![42, 1],
            role: kind.type_name().to_string(),
            name: name.to_string(),
            value: None,
            is_password: false,
            rect: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            control_kind: kind,
            is_control: true,
            is_offscreen: false,
            is_keyboard_focusable: false,
        }
    }

    #[test]
    fn default_filter_requires_hittable_or_focusable() {
        let button = node(ControlKind::Button, "OK");
        let text = node(ControlKind::Text, "label");
        let mut focus_text = text.clone();
        focus_text.is_keyboard_focusable = true;
        assert!(button.passes_filter(Detail::Default));
        assert!(!text.passes_filter(Detail::Default));
        assert!(focus_text.passes_filter(Detail::Default));
        assert!(text.passes_filter(Detail::Dom));
    }

    #[test]
    fn filter_skips_offscreen_and_zero_size() {
        let mut off = node(ControlKind::Button, "x");
        off.is_offscreen = true;
        let mut zero = node(ControlKind::Button, "x");
        zero.rect.w = 0;
        let mut not_control = node(ControlKind::Button, "x");
        not_control.is_control = false;
        assert!(!off.passes_filter(Detail::Default));
        assert!(!zero.passes_filter(Detail::Dom));
        assert!(!not_control.passes_filter(Detail::Dom));
    }

    #[test]
    fn default_cap_250_and_dom_cap_2000() {
        assert_eq!(VIEWPORT_ENVELOPE_ELEMENT_CAP, 20);
        let nodes: Vec<RawNode> = (0..300)
            .map(|i| {
                let mut n = node(ControlKind::Button, "b");
                n.runtime_id = vec![1, i];
                n
            })
            .collect();
        let (els, matched) = filter_nodes(&nodes, Detail::Default);
        assert_eq!(matched, 300);
        assert_eq!(els.len(), 250);
        assert_eq!(els[0].id, "uia:1.0");
        assert_eq!(els[249].id, "uia:1.249");

        let many: Vec<RawNode> = (0..2_100)
            .map(|i| {
                let mut n = node(ControlKind::Text, "t");
                n.runtime_id = vec![2, i];
                n
            })
            .collect();
        let (els, matched) = filter_nodes(&many, Detail::Dom);
        assert_eq!(matched, 2_100);
        assert_eq!(els.len(), 2_000);
    }

    #[test]
    fn extract_caps_and_skips_password() {
        let title = "T".repeat(250);
        let mut doc = node(ControlKind::Document, &"A".repeat(800));
        doc.value = Some("B".repeat(800));
        let mut password = node(ControlKind::Edit, "secret");
        password.is_password = true;
        password.value = Some("hunter2".into());
        let extract = extract_from_nodes(&title, &[password, doc]);
        assert_eq!(extract.title.chars().count(), 200);
        assert_eq!(extract.url, None);
        assert!(extract.cards.is_empty());
        assert_eq!(extract.main_text.chars().count(), 1500);
        assert!(!extract.main_text.contains("secret"));
        assert!(!extract.main_text.contains("hunter2"));
        assert!(extract.main_text.starts_with('A'));
    }

    #[test]
    fn password_element_text_is_null() {
        let mut password = node(ControlKind::Edit, "secret");
        password.is_password = true;
        let el = password.to_element().expect("valid runtime id");
        assert_eq!(el.text, None);
        assert_eq!(el.id, "uia:42.1");
    }

    #[test]
    fn uia_main_text_joins_pieces_with_newline() {
        let a = node(ControlKind::Document, "hello");
        let b = node(ControlKind::Edit, "world");
        let extract = extract_from_nodes("T", &[a, b]);
        assert_eq!(extract.main_text, "hello\nworld");
        assert_eq!(extract.url, None);
        assert!(extract.cards.is_empty());
    }

    #[test]
    fn http_https_url_rejects_chrome_and_about() {
        assert_eq!(
            http_https_url(Some("https://cars.com/search")),
            Some("https://cars.com/search".into())
        );
        assert_eq!(
            http_https_url(Some("http://example.com")),
            Some("http://example.com".into())
        );
        assert_eq!(http_https_url(Some("chrome://extensions")), None);
        assert_eq!(http_https_url(Some("about:blank")), None);
        assert_eq!(http_https_url(Some("")), None);
        assert_eq!(http_https_url(None), None);
    }

    #[test]
    fn empty_runtime_id_is_skipped() {
        let mut n = node(ControlKind::Button, "OK");
        n.runtime_id.clear();
        assert!(!n.passes_filter(Detail::Default));
        assert!(!n.passes_filter(Detail::Dom));
        assert!(n.to_element().is_none());
        let (els, matched) = filter_nodes(&[n], Detail::Default);
        assert_eq!(matched, 0);
        assert!(els.is_empty());
    }

    #[test]
    fn parse_miles_vs_distance_same_blob() {
        let blob = "2024 Toyota Camry 32,145 mi 12 mi away Capital Toyota";
        assert_eq!(parse_miles(blob).as_deref(), Some("32,145 mi"));
        assert_eq!(parse_distance(blob).as_deref(), Some("12 mi away"));
        assert_ne!(parse_miles(blob).as_deref(), Some("12 mi"));
        assert_eq!(parse_miles("12 mi away"), None);
        assert_eq!(parse_miles("12 mi"), None);
        assert_eq!(parse_miles("500 mi"), None);
        assert_eq!(parse_miles("1234 mi").as_deref(), Some("1234 mi"));
        assert_eq!(parse_miles("1,234 miles").as_deref(), Some("1,234 miles"));
        assert_eq!(parse_miles("Shipping from Jacksonville, FL"), None);
    }

    #[test]
    fn parse_distance_away_or_shipping() {
        assert_eq!(
            parse_distance("12 miles away").as_deref(),
            Some("12 miles away")
        );
        let ship = parse_distance("Shipping from Jacksonville, FL").unwrap();
        assert!(ship.to_ascii_lowercase().starts_with("shipping from"));
        assert!(ship.contains("Jacksonville"));
        assert!(ship.len() <= CARD_DISTANCE_CAP);
    }

    #[test]
    fn parse_listing_of_table() {
        assert_eq!(parse_listing_of("1 of 6").as_deref(), Some("1 of 6"));
        assert_eq!(
            parse_listing_of("shown 1 of 6 listings").as_deref(),
            Some("1 of 6")
        );
        assert_eq!(parse_listing_of("none of that"), None);
        assert_eq!(parse_listing_of("of 6"), None);
    }

    #[test]
    fn parse_result_and_local_counts() {
        assert_eq!(parse_result_count("323 cars").as_deref(), Some("323 cars"));
        assert_eq!(
            parse_result_count("0 matches nearby").as_deref(),
            Some("0 matches")
        );
        assert_eq!(
            parse_result_count("10,000+ matches").as_deref(),
            Some("10,000+ matches")
        );
        assert_eq!(
            parse_local_matches("6 local dealers").as_deref(),
            Some("6 local")
        );
        assert_eq!(parse_local_matches("shop locally"), None);
    }

    #[test]
    fn parse_empty_state_positives_and_negatives() {
        for phrase in [
            "nothing fits those filters",
            "we couldn't find",
            "we couldnt find",
            "0 matches",
            "no cars match",
            "no results",
            "try a larger radius",
            "expand your search",
        ] {
            let got = parse_empty_state(&format!("Sorry, {phrase} right now.")).unwrap();
            assert!(
                got.to_ascii_lowercase().contains(phrase),
                "expected {phrase} in {got}"
            );
            assert!(got.chars().count() <= EMPTY_STATE_CAP);
        }
        assert!(parse_empty_state("Continue shopping").is_none());
        assert!(parse_empty_state("i'm not a robot").is_none());
        assert!(parse_empty_state("cloudflare challenge").is_none());
        for heading in [
            "10 matches",
            "100 matches",
            "320 matches",
            "10,000+ matches",
        ] {
            assert!(
                parse_empty_state(heading).is_none(),
                "heading {heading} must not be empty-state"
            );
        }
        assert!(
            parse_empty_state("0 matches nearby")
                .unwrap()
                .to_ascii_lowercase()
                .contains("0 matches")
        );
    }

    #[test]
    fn parse_dealer_from_card_text_not_footer() {
        let card = "2024 Toyota Camry 32,145 mi 12 mi away Capital Toyota $19,999 1 of 6";
        assert_eq!(
            parse_dealer(card, "2024 Toyota Camry", "$19,999").as_deref(),
            Some("Capital Toyota")
        );
        assert_eq!(
            parse_dealer("2024 Camry Capital Toyota", "2024 Camry", "").as_deref(),
            Some("Capital Toyota")
        );
        assert!(
            parse_dealer("2024 Toyota Camry $19,999", "2024 Toyota Camry", "$19,999").is_none()
        );
        let mut extract = Extract {
            title: "Results".into(),
            url: None,
            main_text: "Capital Toyota footer nav".into(),
            cards: vec![Card {
                title: "2024 Camry".into(),
                price: "$19,999".into(),
                href: "https://cars.com/1".into(),
                rect: Rect {
                    x: 1,
                    y: 1,
                    w: 2,
                    h: 2,
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        enrich_listing(&mut extract);
        assert!(extract.cards[0].dealer.is_none());
    }

    #[test]
    fn parse_zip_radius_from_cars_url_and_heading() {
        let url = "https://www.cars.com/shopping/results/?stock_type=used&makes[]=toyota&models[]=toyota-camry&list_price_max=20000&maximum_distance=50&zip=32309&year_min=2024";
        let (zip, radius) = parse_zip_radius(url, "");
        assert_eq!(zip.as_deref(), Some("32309"));
        assert_eq!(radius.as_deref(), Some("50 mi"));
        let (zip, radius) = parse_zip_radius("", "Showing results within 50 miles of 32309");
        assert_eq!(zip.as_deref(), Some("32309"));
        assert_eq!(radius.as_deref(), Some("50 mi"));
    }

    #[test]
    fn enrich_listing_fills_empty_only_and_does_not_guess() {
        let mut extract = Extract {
            title: "Results".into(),
            url: Some(
                "https://www.cars.com/shopping/results/?zip=32309&maximum_distance=50".into(),
            ),
            main_text: "323 cars. 6 local. Nothing fits those filters. 32,145 mi 12 mi away 1 of 6 Capital Toyota footer".into(),
            cards: vec![
                Card {
                    title: "2024 Camry".into(),
                    price: "$19,999".into(),
                    href: "https://cars.com/1".into(),
                    rect: Rect {
                        x: 1,
                        y: 1,
                        w: 2,
                        h: 2,
                    },
                    miles: Some("JS-MI".into()),
                    dealer: Some("JS-DEALER".into()),
                    distance: Some("JS-DIST".into()),
                    listing_of: Some("2 of 2".into()),
                },
                Card {
                    title: "45,000 mi 8 mi away 1 of 6 sedan".into(),
                    price: "$18,000".into(),
                    href: "https://cars.com/2".into(),
                    rect: Rect {
                        x: 1,
                        y: 1,
                        w: 2,
                        h: 2,
                    },
                    ..Default::default()
                },
            ],
            result_count: Some("JS-COUNT".into()),
            zip: Some("00000".into()),
            ..Default::default()
        };
        enrich_listing(&mut extract);
        assert_eq!(extract.result_count.as_deref(), Some("JS-COUNT"));
        assert_eq!(extract.zip.as_deref(), Some("00000"));
        assert_eq!(extract.radius.as_deref(), Some("50 mi"));
        assert_eq!(extract.local_matches.as_deref(), Some("6 local"));
        assert!(
            extract
                .empty_state
                .as_deref()
                .unwrap()
                .to_ascii_lowercase()
                .contains("nothing fits those filters")
        );
        assert_eq!(extract.cards[0].miles.as_deref(), Some("JS-MI"));
        assert_eq!(extract.cards[0].dealer.as_deref(), Some("JS-DEALER"));
        assert_eq!(extract.cards[0].distance.as_deref(), Some("JS-DIST"));
        assert_eq!(extract.cards[0].listing_of.as_deref(), Some("2 of 2"));
        assert_eq!(extract.cards[1].miles.as_deref(), Some("45,000 mi"));
        assert_eq!(extract.cards[1].distance.as_deref(), Some("8 mi away"));
        assert_eq!(extract.cards[1].listing_of.as_deref(), Some("1 of 6"));
        assert!(extract.cards[1].dealer.is_none());
    }

    #[test]
    fn enrich_does_not_stamp_page_miles_or_count_from_cards() {
        let mut extract = Extract {
            title: "Results".into(),
            url: None,
            main_text: "32,145 mi on the lot. 12 mi away.".into(),
            cards: vec![
                Card {
                    title: "2024 Camry".into(),
                    price: "$19,999".into(),
                    href: "https://cars.com/1".into(),
                    rect: Rect {
                        x: 1,
                        y: 1,
                        w: 2,
                        h: 2,
                    },
                    ..Default::default()
                },
                Card {
                    title: "Other Camry".into(),
                    price: "$18,000".into(),
                    href: "https://cars.com/2".into(),
                    rect: Rect {
                        x: 1,
                        y: 1,
                        w: 2,
                        h: 2,
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        enrich_listing(&mut extract);
        assert!(extract.cards[0].miles.is_none());
        assert!(extract.cards[1].miles.is_none());
        assert!(extract.cards.iter().all(|c| c.dealer.is_none()));
        assert!(extract.result_count.is_none());
    }

    #[test]
    fn empty_listing_fields_omitted_from_extract_json() {
        let extract = Extract {
            title: "T".into(),
            url: None,
            main_text: String::new(),
            cards: vec![Card {
                title: "c".into(),
                price: "$1".into(),
                href: "https://example.com".into(),
                rect: Rect {
                    x: 1,
                    y: 1,
                    w: 2,
                    h: 2,
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let v = serde_json::to_value(&extract).unwrap();
        for key in [
            "result_count",
            "local_matches",
            "empty_state",
            "zip",
            "radius",
            "dialogs",
        ] {
            assert!(v.get(key).is_none(), "{key} should be omitted");
        }
        let card = &v["cards"][0];
        for key in ["miles", "dealer", "distance", "of"] {
            assert!(card.get(key).is_none(), "card.{key} should be omitted");
        }
    }

    #[test]
    fn uia_extract_enriches_empty_state_without_cards() {
        let doc = node(ControlKind::Document, "Nothing fits those filters");
        let extract = extract_from_nodes("Results", &[doc]);
        assert!(extract.cards.is_empty());
        assert!(
            extract
                .empty_state
                .as_deref()
                .unwrap()
                .to_ascii_lowercase()
                .contains("nothing fits those filters")
        );
    }
}
