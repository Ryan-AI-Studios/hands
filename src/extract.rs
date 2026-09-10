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
pub const CARD_KIND_CAP: usize = 12;
pub const CARD_DELIVERY_CAP: usize = 40;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
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
            kind: None,
            delivery: None,
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
    #[serde(skip)]
    pub cards_walked: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingMeta {
    pub result_count: Option<String>,
    pub local_matches: Option<String>,
    pub empty_state: Option<String>,
    pub zip: Option<String>,
    pub radius: Option<String>,
    pub cards_walked: usize,
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
        cards_walked: listing.cards_walked,
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
            if is_paren_mi_span(text, start, after + unit_len, digits) {
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
    if let Some(paren) = parse_paren_mi_distance(text) {
        return Some(paren);
    }
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
    let query_dist = query_value(url, "maximum_distance").map(str::trim);
    let mut radius = match query_dist {
        Some(d) if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) => {
            Some(take_chars(&format!("{} mi", d), RADIUS_CAP))
        }
        _ => None,
    };
    let (text_zip, text_radius) = parse_within_zip_radius(text);
    if zip.as_ref().is_none_or(|s| s.is_empty()) {
        zip = text_zip;
    }
    if radius.as_ref().is_none_or(|s| s.is_empty()) {
        radius = text_radius;
    }
    if radius.as_ref().is_none_or(|s| s.is_empty())
        && query_dist.is_some_and(|d| d.eq_ignore_ascii_case("all"))
    {
        radius = Some(take_chars("all", RADIUS_CAP));
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
        if card.kind.is_none() {
            card.kind = parse_card_kind(&card.title);
        }
        if card.delivery.is_none() {
            card.delivery = parse_delivery(&card.title);
        }
        card.dealer = card.dealer.take().and_then(|d| sanitize_dealer(&d));
        if price_looks_monthly(&card.price, &card.title) {
            card.price = parse_listing_price(&card.title).unwrap_or_default();
        }
    }
}

/// Dealer from a **card-scoped** blob only (already-collected card innerText).
/// Strip title / price / miles / distance / delivery / `of` / junk; leftover is the dealer.
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
    while let Some(delivery) = parse_delivery(&rest) {
        rest = rest.replace(&delivery, " ");
    }
    if let Some(of) = parse_listing_of(&rest) {
        rest = rest.replace(&of, " ");
    }
    sanitize_dealer(&rest)
}

/// Junk-strip a dealer **string** (JS leftover or itemprop). Production ingest honesty.
pub fn sanitize_dealer(dealer_string: &str) -> Option<String> {
    let mut rest = strip_review_parens(dealer_string);
    rest = strip_junk_phrases(&rest);
    rest = strip_price_tokens(&rest);
    rest = collapse_ws(&rest);
    rest = trim_punct(&rest);
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
    if leftover_contains_junk(&lower) {
        return None;
    }
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() == 1 && !is_dealerish_token(tokens[0]) {
        return None;
    }
    Some(take_chars(&rest, CARD_DEALER_CAP))
}

pub fn parse_listing_price(text: &str) -> Option<String> {
    let mut hits: Vec<(usize, usize, bool)> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        if (c == '$' || c == '€' || c == '£')
            && let Some((start, end, grouped)) = currency_amount_at(text, i)
        {
            if !should_skip_price_hit(text, start, end) {
                hits.push((start, end, grouped));
            }
            i = end;
            continue;
        }
        i += c.len_utf8();
    }
    if let Some(&(start, end, _)) = hits.iter().find(|h| h.2) {
        return Some(take_chars(&text[start..end], CARD_PRICE_FALLBACK_CAP));
    }
    if let Some(&(start, end, _)) = hits.first() {
        return Some(take_chars(&text[start..end], CARD_PRICE_FALLBACK_CAP));
    }
    parse_plain_decimal_price(text)
}

const CARD_PRICE_FALLBACK_CAP: usize = 24;

pub fn parse_delivery(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let candidates = [
        delivery_span(text, &lower, "available for delivery to"),
        delivery_span(text, &lower, "est. shipping"),
        delivery_shipping_dollar(text, &lower),
        delivery_deliver_to(text, &lower),
    ];
    candidates
        .into_iter()
        .flatten()
        .min_by_key(|(start, _)| *start)
        .map(|(_, phrase)| take_chars(&phrase, CARD_DELIVERY_CAP))
}

pub fn parse_card_kind(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if has_recommended_phrase(&lower) {
        return Some("recommended".into());
    }
    if has_ship_marker(text, &lower) {
        return Some("ship".into());
    }
    if is_in_radius_distance(text) {
        return Some("local".into());
    }
    None
}

pub fn infer_card_kind(card: &Card) -> Option<String> {
    if let Some(kind) = card
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        return Some(take_chars(kind, CARD_KIND_CAP));
    }
    if has_recommended_phrase(&card.title.to_ascii_lowercase()) {
        return Some("recommended".into());
    }
    let delivery_text = card.delivery.as_deref().unwrap_or("");
    let delivery_l = delivery_text.to_ascii_lowercase();
    let distance_l = card.distance.as_deref().unwrap_or("").to_ascii_lowercase();
    let title_l = card.title.to_ascii_lowercase();
    if has_ship_marker(delivery_text, &delivery_l)
        || has_ship_marker(&card.title, &title_l)
        || distance_l.contains("shipping from")
        || distance_l.contains("deliver to")
    {
        return Some("ship".into());
    }
    if card.distance.as_deref().is_some_and(is_in_radius_distance)
        || is_in_radius_distance(&card.title)
    {
        return Some("local".into());
    }
    None
}

pub fn prefer_local_cards(cards: Vec<Card>, cap: usize) -> Vec<Card> {
    let mut local = Vec::new();
    let mut unknown = Vec::new();
    let mut ship = Vec::new();
    let mut rec = Vec::new();
    for card in cards {
        match card.kind.as_deref() {
            Some("local") => local.push(card),
            Some("ship") => ship.push(card),
            Some("recommended") => rec.push(card),
            _ => unknown.push(card),
        }
    }
    let mut out = Vec::with_capacity(cap.min(local.len() + unknown.len() + ship.len() + rec.len()));
    out.extend(local);
    out.extend(unknown);
    out.extend(ship);
    out.extend(rec);
    out.truncate(cap);
    out
}

const DEALER_JUNK_PHRASES: &[&str] = &[
    "american-made index",
    "american made index",
    "available for delivery",
    "no price analysis",
    "get pre-approved",
    "check availability",
    "days on cars.com",
    "you may also like",
    "contact dealer",
    "home delivery",
    "est. shipping",
    "free carfax",
    "view details",
    "get financing",
    "price drop",
    "great deal",
    "good deal",
    "fair deal",
    "high price",
    "deliver to",
    "shipping to",
    "per month",
    "see more",
    "recommended",
    "autocheck",
    "sponsored",
    "shipping",
    "reviews",
    "ratings",
    "review",
    "rating",
    "stars",
    "star",
    "est.",
    "/mo",
];

const PRICE_SKIP_TOKENS: &[&str] = &["est", "est.", "drop", "save", "off", "discount"];

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

fn strip_junk_phrases(text: &str) -> String {
    let mut rest = text.to_string();
    loop {
        let lower = rest.to_ascii_lowercase();
        let mut found: Option<(usize, usize)> = None;
        for phrase in DEALER_JUNK_PHRASES {
            if let Some(idx) = find_bounded_phrase(&lower, phrase) {
                let end = idx + phrase.len();
                match found {
                    Some((s, _)) if idx >= s => {}
                    _ => found = Some((idx, end)),
                }
            }
        }
        let Some((start, end)) = found else {
            break;
        };
        rest.replace_range(start..end, " ");
    }
    rest
}

fn leftover_contains_junk(lower: &str) -> bool {
    DEALER_JUNK_PHRASES
        .iter()
        .any(|phrase| find_bounded_phrase(lower, phrase).is_some())
}

fn find_bounded_phrase(lower: &str, phrase: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut search = 0;
    while search <= lower.len().saturating_sub(phrase.len()) {
        let rel = lower[search..].find(phrase)?;
        let idx = search + rel;
        let after = idx + phrase.len();
        if phrase == "/mo" {
            return Some(idx);
        }
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return Some(idx);
        }
        search = idx + 1;
    }
    None
}

fn strip_review_parens(text: &str) -> String {
    let mut rest = text.to_string();
    while let Some((start, end)) = find_review_paren(&rest) {
        rest.replace_range(start..end, " ");
    }
    rest
}

fn find_review_paren(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let after_open = skip_ws_bytes(text, i + 1);
            if let Some((n_start, n_end, digits, _)) = next_number(text, after_open)
                && n_start == after_open
                && digits > 0
            {
                let after_num = skip_ws_bytes(text, n_end);
                let (is_reviews, word_len) = if is_word_at(&text[after_num..], "reviews") {
                    (true, 7usize)
                } else if is_word_at(&text[after_num..], "review") {
                    (true, 6usize)
                } else {
                    (false, 0usize)
                };
                if is_reviews {
                    let after_word = skip_ws_bytes(text, after_num + word_len);
                    if after_word < bytes.len() && bytes[after_word] == b')' {
                        return Some((i, after_word + 1));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn trim_punct(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
}

fn is_dealerish_token(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    matches!(lower.as_str(), "carmax" | "carvana" | "vroom")
        || lower.contains("auto")
        || lower.contains("motor")
}

fn is_paren_mi_span(text: &str, num_start: usize, unit_end: usize, digits: usize) -> bool {
    if !(1..=3).contains(&digits) {
        return false;
    }
    let before = skip_ws_back(text, num_start);
    if before == 0 || text.as_bytes()[before - 1] != b'(' {
        return false;
    }
    let after = skip_ws_bytes(text, unit_end);
    after < text.len() && text.as_bytes()[after] == b')'
}

fn skip_ws_back(text: &str, mut i: usize) -> usize {
    while i > 0 {
        let Some(c) = text[..i].chars().next_back() else {
            break;
        };
        if !c.is_whitespace() {
            break;
        }
        i -= c.len_utf8();
    }
    i
}

fn parse_paren_mi_distance(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some((start, num_end, digits, _)) = next_number(text, from) {
        if (1..=3).contains(&digits) {
            let after = skip_ws_bytes(text, num_end);
            if let Some(unit_len) = match_mi_unit(&text[after..]) {
                let unit_end = after + unit_len;
                let close_at = skip_ws_bytes(text, unit_end);
                if close_at < text.len() && text.as_bytes()[close_at] == b')' {
                    let open_at = skip_ws_back(text, start);
                    if open_at > 0 && text.as_bytes()[open_at - 1] == b'(' {
                        let paren_start = open_at - 1;
                        let paren_end = close_at + 1;
                        if let Some(extended) = extend_city_st(text, paren_start, paren_end) {
                            return Some(take_chars(&extended, CARD_DISTANCE_CAP));
                        }
                        let phrase = text[paren_start..paren_end].trim();
                        if !phrase.is_empty() {
                            return Some(take_chars(phrase, CARD_DISTANCE_CAP));
                        }
                    }
                }
            }
        }
        from = num_end;
    }
    None
}

fn extend_city_st(text: &str, paren_start: usize, paren_end: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let i = skip_ws_back(text, paren_start);
    if i < 2 {
        return None;
    }
    if !bytes[i - 1].is_ascii_alphabetic() || !bytes[i - 2].is_ascii_alphabetic() {
        return None;
    }
    if i >= 3 && bytes[i - 3].is_ascii_alphabetic() {
        return None;
    }
    let st_start = i - 2;
    let mut j = skip_ws_back(text, st_start);
    if j == 0 || bytes[j - 1] != b',' {
        return None;
    }
    j = skip_ws_back(text, j - 1);
    let city_end = j;
    while j > 0 && bytes[j - 1].is_ascii_alphanumeric() {
        j -= 1;
    }
    if city_end <= j {
        return None;
    }
    let phrase = text[j..paren_end].trim();
    if phrase.is_empty() {
        None
    } else {
        Some(phrase.to_string())
    }
}

fn has_recommended_phrase(lower: &str) -> bool {
    lower.contains("you may also like")
        || lower.contains("outside your search")
        || lower.contains("outside your area")
        || find_bounded_phrase(lower, "recommended").is_some()
}

fn has_ship_marker(text: &str, lower: &str) -> bool {
    if lower.contains("shipping from") || lower.contains("deliver to") {
        return true;
    }
    delivery_shipping_dollar(text, lower).is_some()
}

fn is_in_radius_distance(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains(" mi away") || lower.contains(" miles away") {
        return true;
    }
    parse_paren_mi_distance(text).is_some()
}

fn delivery_span(text: &str, lower: &str, needle: &str) -> Option<(usize, String)> {
    let idx = lower.find(needle)?;
    let line = text[idx..]
        .split(['\n', '\r'])
        .next()
        .unwrap_or(&text[idx..]);
    let phrase = line.trim();
    if phrase.is_empty() {
        None
    } else {
        Some((idx, phrase.to_string()))
    }
}

fn delivery_shipping_dollar(text: &str, lower: &str) -> Option<(usize, String)> {
    let mut search = 0;
    while let Some(rel) = lower[search..].find("shipping") {
        let idx = search + rel;
        let after_word = idx + "shipping".len();
        let after = skip_ws_bytes(text, after_word);
        if is_word_at(&text[after..], "from") {
            search = idx + 1;
            continue;
        }
        if text
            .get(after..)
            .is_some_and(|s| s.starts_with('$') || s.starts_with('€') || s.starts_with('£'))
            && let Some((_, amt_end, _)) = currency_amount_at(text, after)
        {
            let phrase = text[idx..amt_end].trim();
            if !phrase.is_empty() {
                return Some((idx, phrase.to_string()));
            }
        }
        search = idx + 1;
    }
    None
}

fn delivery_deliver_to(text: &str, lower: &str) -> Option<(usize, String)> {
    let idx = lower.find("deliver to")?;
    let after = skip_ws_bytes(text, idx + "deliver to".len());
    let zip = take_zip_at(text, after)?;
    let end = after + zip.len();
    let phrase = text[idx..end].trim();
    if phrase.is_empty() {
        None
    } else {
        Some((idx, phrase.to_string()))
    }
}

fn price_looks_monthly(price: &str, title: &str) -> bool {
    if price.trim().is_empty() {
        return false;
    }
    let p = price.to_ascii_lowercase();
    if p.contains("/mo") || p.contains("est.") {
        return true;
    }
    if let Some(idx) = title.find(price) {
        let after = skip_ws_bytes(title, idx + price.len());
        let rest = &title[after..];
        if rest.starts_with("/mo") {
            return true;
        }
        if let Some(stripped) = rest.strip_prefix('/') {
            let t = stripped.trim_start();
            if is_word_at(t, "mo") {
                return true;
            }
        }
        if is_word_at(rest, "mo") {
            return true;
        }
    }
    false
}

fn currency_amount_at(text: &str, byte_i: usize) -> Option<(usize, usize, bool)> {
    let rest = text.get(byte_i..)?;
    let mut chars = rest.chars();
    let sign = chars.next()?;
    if sign != '$' && sign != '€' && sign != '£' {
        return None;
    }
    let mut end = byte_i + sign.len_utf8();
    let mut grouped = false;
    let mut saw_digit = false;
    for ch in text[end..].chars() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            end += 1;
        } else if ch == ',' {
            grouped = true;
            end += 1;
        } else if ch == '.' {
            end += 1;
        } else {
            break;
        }
    }
    if !saw_digit {
        return None;
    }
    Some((byte_i, end, grouped))
}

fn should_skip_price_hit(text: &str, start: usize, end: usize) -> bool {
    let after = skip_ws_bytes(text, end);
    let rest = &text[after..];
    if rest.starts_with("/mo") {
        return true;
    }
    if let Some(stripped) = rest.strip_prefix('/') {
        let t = stripped.trim_start();
        if is_word_at(t, "mo") {
            return true;
        }
    }
    if is_word_at(rest, "mo") {
        return true;
    }
    if prev_whole_token(text, start).is_some_and(is_price_skip_token) {
        return true;
    }
    let Some((after1, tok1)) = next_whole_token(text, end) else {
        return false;
    };
    if is_price_skip_token(tok1) {
        return true;
    }
    if let Some((_, tok2)) = next_whole_token(text, after1)
        && is_price_skip_token(tok2)
    {
        return true;
    }
    false
}

fn is_price_skip_token(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    PRICE_SKIP_TOKENS.iter().any(|s| lower == *s)
}

fn prev_whole_token(text: &str, pos: usize) -> Option<&str> {
    let i = skip_ws_back(text, pos);
    if i == 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut start = i;
    if bytes[i - 1] == b'.' {
        start -= 1;
        while start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            start -= 1;
        }
        if start < i {
            return Some(&text[start..i]);
        }
        return None;
    }
    while start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
        start -= 1;
    }
    if start < i {
        Some(&text[start..i])
    } else {
        None
    }
}

fn next_whole_token(text: &str, pos: usize) -> Option<(usize, &str)> {
    let start = skip_ws_bytes(text, pos);
    if start >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut end = start;
    if bytes[start] == b'.' {
        return None;
    }
    while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        end += 1;
    }
    let mut tok_end = end;
    if tok_end < bytes.len() && bytes[tok_end] == b'.' {
        let candidate = &text[start..=tok_end];
        if candidate.eq_ignore_ascii_case("est.") {
            tok_end += 1;
            return Some((tok_end, &text[start..tok_end]));
        }
    }
    if end > start {
        Some((end, &text[start..end]))
    } else {
        None
    }
}

fn parse_plain_decimal_price(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b',') {
                i += 1;
            }
            if i + 2 < bytes.len()
                && bytes[i] == b'.'
                && bytes[i + 1].is_ascii_digit()
                && bytes[i + 2].is_ascii_digit()
            {
                let dec_end = i + 3;
                let after_ok = dec_end >= bytes.len() || !bytes[dec_end].is_ascii_digit();
                if after_ok && i > start {
                    return Some(take_chars(&text[start..dec_end], CARD_PRICE_FALLBACK_CAP));
                }
            }
        }
        i += 1;
    }
    None
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
    fn parse_dealer_strips_live_junk_and_keeps_capital_toyota() {
        assert_eq!(parse_dealer("Peter", "", ""), None);
        assert_eq!(
            parse_dealer(". Est. /mo Great Deal American-Made Index Peter", "", ""),
            None
        );
        let ship = "2024 Camry Capital Toyota Est. shipping $399 Deliver to 32309";
        let dealer = parse_dealer(ship, "2024 Camry", "");
        assert_eq!(dealer.as_deref(), Some("Capital Toyota"));
        assert_ne!(dealer.as_deref(), Some("Deliver to 32309"));
        assert_ne!(dealer.as_deref(), Some("Est. shipping"));
        assert_eq!(parse_dealer("Deliver to 32309", "", ""), None);
        let card = "2024 Toyota Camry 32,145 mi 12 mi away Capital Toyota $19,999 1 of 6";
        assert_eq!(
            parse_dealer(card, "2024 Toyota Camry", "$19,999").as_deref(),
            Some("Capital Toyota")
        );
        assert_eq!(
            parse_dealer(
                "2024 Camry Capital Toyota Tallahassee, FL (12 mi) $19,999",
                "2024 Camry",
                "$19,999"
            )
            .as_deref(),
            Some("Capital Toyota")
        );
        assert_eq!(
            parse_dealer(
                "2024 Camry Tallahassee, FL (12 mi) $19,999",
                "2024 Camry",
                "$19,999"
            ),
            None
        );
        assert_eq!(sanitize_dealer("American-Made"), None);
        assert_eq!(sanitize_dealer("Peter"), None);
        assert_eq!(sanitize_dealer("CarMax").as_deref(), Some("CarMax"));
    }

    #[test]
    fn parse_listing_price_skips_monthly_and_keeps_crest_office() {
        assert_eq!(
            parse_listing_price("2024 Camry Est. $312/mo Great Deal $19,999").as_deref(),
            Some("$19,999")
        );
        assert_eq!(
            parse_listing_price("Crest Motors $9,995").as_deref(),
            Some("$9,995")
        );
        assert_eq!(
            parse_listing_price("Office Square Motors $19,999").as_deref(),
            Some("$19,999")
        );
        assert_eq!(parse_listing_price("$1,000 price drop"), None);
        assert_eq!(parse_listing_price("$500 off"), None);
        assert_eq!(
            parse_listing_price("Est. $312/mo Great Deal $19,999").as_deref(),
            Some("$19,999")
        );
    }

    #[test]
    fn parse_distance_paren_and_not_shipping_fee() {
        assert_eq!(parse_distance("(12 mi)").as_deref(), Some("(12 mi)"));
        assert_eq!(
            parse_distance("Tallahassee, FL (12 mi)").as_deref(),
            Some("Tallahassee, FL (12 mi)")
        );
        let greedy = parse_distance("Capital Toyota Tallahassee, FL (12 mi)").unwrap();
        assert_ne!(greedy, "Capital Toyota Tallahassee, FL (12 mi)");
        assert!(
            greedy == "(12 mi)" || greedy == "Tallahassee, FL (12 mi)",
            "got {greedy}"
        );
        assert!(!greedy.contains("Capital Toyota"));
        assert_eq!(parse_distance("Shipping $399 to 32309"), None);
        assert_eq!(parse_miles("Shipping $399 to 32309"), None);
        assert_eq!(parse_miles("(12 mi)"), None);
        assert_eq!(parse_miles("(5 mi)"), None);
    }

    #[test]
    fn parse_delivery_and_kind_home_delivery_stays_local() {
        assert_eq!(
            parse_delivery("Est. shipping $399").as_deref(),
            Some("Est. shipping $399")
        );
        assert_eq!(
            parse_delivery("Shipping $399").as_deref(),
            Some("Shipping $399")
        );
        assert_eq!(
            parse_delivery("Deliver to 32309").as_deref(),
            Some("Deliver to 32309")
        );
        assert_eq!(parse_delivery("home delivery"), None);
        assert_eq!(
            parse_card_kind("12 mi away home delivery").as_deref(),
            Some("local")
        );
        assert_eq!(
            parse_card_kind("You may also like Shipping $399").as_deref(),
            Some("recommended")
        );
        assert_eq!(
            parse_card_kind("Shipping from Jacksonville, FL").as_deref(),
            Some("ship")
        );
        assert_eq!(parse_card_kind("2024 Camry SE"), None);
        let local = Card {
            title: "2024 Camry".into(),
            distance: Some("12 mi away".into()),
            ..Default::default()
        };
        assert_eq!(infer_card_kind(&local).as_deref(), Some("local"));
        let ship = Card {
            title: "2024 Camry".into(),
            delivery: Some("Shipping $399".into()),
            ..Default::default()
        };
        assert_eq!(infer_card_kind(&ship).as_deref(), Some("ship"));
        let paren = Card {
            title: "2024 Camry".into(),
            distance: Some("(12 mi)".into()),
            ..Default::default()
        };
        assert_eq!(infer_card_kind(&paren).as_deref(), Some("local"));
        let home_local = Card {
            title: "2024 Camry".into(),
            distance: Some("12 mi away".into()),
            delivery: Some("available for delivery to 32309".into()),
            ..Default::default()
        };
        assert_eq!(infer_card_kind(&home_local).as_deref(), Some("local"));
        assert_eq!(
            parse_card_kind("12 mi away available for delivery to 32309").as_deref(),
            Some("local")
        );
        assert_eq!(sanitize_dealer("American-Made"), None);
    }

    #[test]
    fn prefer_local_cards_packs_locals_first() {
        fn c(title: &str, kind: Option<&str>) -> Card {
            Card {
                title: title.into(),
                kind: kind.map(str::to_string),
                ..Default::default()
            }
        }
        let mut cards = Vec::new();
        for i in 0..8 {
            cards.push(c(&format!("ship {i}"), Some("ship")));
        }
        for i in 0..5 {
            cards.push(c(&format!("local {i}"), Some("local")));
        }
        let packed = prefer_local_cards(cards, 8);
        assert_eq!(packed.len(), 8);
        assert!(
            packed
                .iter()
                .take(5)
                .all(|x| x.kind.as_deref() == Some("local"))
        );
        assert!(
            packed
                .iter()
                .skip(5)
                .all(|x| x.kind.as_deref() == Some("ship"))
        );
        assert_eq!(packed[0].title, "local 0");
        assert_eq!(packed[5].title, "ship 0");
        let mut nine = Vec::new();
        for i in 0..9 {
            nine.push(c(&format!("car {i}"), Some("local")));
        }
        let eight = prefer_local_cards(nine, 8);
        assert_eq!(eight.len(), 8);
        assert_eq!(eight[0].title, "car 0");
        assert_eq!(eight[7].title, "car 7");
    }

    #[test]
    fn enrich_clears_js_junk_dealer() {
        let mut extract = Extract {
            title: "Results".into(),
            url: None,
            main_text: "footer Capital Toyota".into(),
            cards: vec![Card {
                title: "2024 Camry".into(),
                price: "$312/mo".into(),
                href: "https://cars.com/1".into(),
                rect: Rect {
                    x: 1,
                    y: 1,
                    w: 2,
                    h: 2,
                },
                dealer: Some(". Est. /mo Great Deal Peter".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        enrich_listing(&mut extract);
        assert!(extract.cards[0].dealer.is_none());
        assert_ne!(extract.cards[0].price, "$312/mo");
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
    fn element_id_is_runtime_id_dot_join_not_walk_order() {
        let mut target = node(ControlKind::Edit, "address");
        target.runtime_id = vec![42, 591400, 4, 0, 0, 301];
        assert_eq!(
            target.element_id().as_deref(),
            Some("uia:42.591400.4.0.0.301")
        );
        assert_ne!(target.element_id().as_deref(), Some("uia:0"));
        assert_ne!(target.element_id().as_deref(), Some("uia:6"));

        // Seventh node still encodes RuntimeId, not walk order.
        let mut nodes: Vec<RawNode> = (0..6)
            .map(|i| {
                let mut n = node(ControlKind::Button, "b");
                n.runtime_id = vec![1, i];
                n
            })
            .collect();
        nodes.push(target);
        let (els, matched) = filter_nodes(&nodes, Detail::Dom);
        assert_eq!(matched, 7);
        assert_eq!(els.len(), 7);
        assert_eq!(els[6].id, "uia:42.591400.4.0.0.301");
        assert_ne!(els[6].id, "uia:6");
        assert_ne!(els[6].id, "uia:0");
        assert_eq!(els[6].id, nodes[6].element_id().unwrap());
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
        let nationwide = "https://www.cars.com/shopping/results/?maximum_distance=all&zip=32309";
        let (zip, radius) = parse_zip_radius(nationwide, "");
        assert_eq!(zip.as_deref(), Some("32309"));
        assert_eq!(radius.as_deref(), Some("all"));
        assert_ne!(radius.as_deref(), Some("all mi"));
        for token in ["All", "ALL"] {
            let url = format!(
                "https://www.cars.com/shopping/results/?maximum_distance={token}&zip=32309"
            );
            let (zip, radius) = parse_zip_radius(&url, "");
            assert_eq!(zip.as_deref(), Some("32309"), "token={token}");
            assert_eq!(radius.as_deref(), Some("all"), "token={token}");
        }
        let (zip, radius) =
            parse_zip_radius(nationwide, "Showing results within 50 miles of 32309");
        assert_eq!(zip.as_deref(), Some("32309"));
        assert_eq!(radius.as_deref(), Some("50 mi"));
        let garbage = "https://www.cars.com/shopping/results/?maximum_distance=foo&zip=32309";
        let (zip, radius) = parse_zip_radius(garbage, "");
        assert_eq!(zip.as_deref(), Some("32309"));
        assert_eq!(radius, None);
        let digit_wins = "https://www.cars.com/shopping/results/?maximum_distance=50&zip=32309";
        let (zip, radius) =
            parse_zip_radius(digit_wins, "Showing results within 100 miles of 99999");
        assert_eq!(zip.as_deref(), Some("32309"));
        assert_eq!(radius.as_deref(), Some("50 mi"));
    }

    #[test]
    fn parse_zip_radius_does_not_unguarded_suffix_query() {
        let src = include_str!("extract.rs");
        let start = src
            .find("pub fn parse_zip_radius(")
            .expect("pub fn parse_zip_radius(");
        let rest = &src[start..];
        let end = rest
            .find("pub fn enrich_listing(")
            .expect("pub fn enrich_listing(");
        let slice = &rest[..end];
        assert!(
            !slice.contains("#[cfg(test)]") && !slice.contains("mod tests"),
            "slice must not include tests"
        );
        let old = concat!(
            ".map(|d| take_chars(&format!(\"{} mi\", d.trim()), ",
            "RADIUS_CAP))"
        );
        assert!(
            !slice.contains(old),
            "parse_zip_radius must not suffix every maximum_distance"
        );
        assert!(
            slice.contains("is_ascii_digit"),
            "parse_zip_radius must digit-check the query"
        );
        assert!(
            slice.contains("eq_ignore_ascii_case(\"all\")"),
            "parse_zip_radius must stamp canonical all"
        );
        assert!(
            !slice.contains("all mi"),
            "parse_zip_radius must not stamp all mi"
        );
    }

    #[test]
    fn collect_listing_meta_does_not_unguarded_suffix_query() {
        let src = include_str!("../extension/content.js");
        let start = src
            .find("function collectListingMeta(")
            .expect("function collectListingMeta(");
        let rest = &src[start..];
        let end = rest
            .find("function parseResultCount(")
            .expect("function parseResultCount(");
        let slice = &rest[..end];
        let old = concat!("if (maxDist) {\n    radius = maxDist + ", "\" mi\";\n  }");
        assert!(
            !slice.contains(old),
            "collectListingMeta must not suffix every maxDist"
        );
        assert!(
            slice.contains(r"/^\d+$/"),
            "collectListingMeta must digit-check maxDist"
        );
        assert!(
            slice.contains(r#"radius = "all""#),
            "collectListingMeta must stamp canonical all"
        );
        assert!(
            !slice.contains("all mi"),
            "collectListingMeta must not stamp all mi"
        );
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
                    dealer: Some("JS Dealer".into()),
                    distance: Some("JS-DIST".into()),
                    listing_of: Some("2 of 2".into()),
                    kind: None,
                    delivery: None,
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
        assert_eq!(extract.cards[0].dealer.as_deref(), Some("JS Dealer"));
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
        for key in ["miles", "dealer", "distance", "of", "kind", "delivery"] {
            assert!(card.get(key).is_none(), "card.{key} should be omitted");
        }
        assert!(
            v.get("cards_walked").is_none(),
            "Extract.cards_walked must not appear in extract JSON"
        );
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
