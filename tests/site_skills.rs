//! Test-only validator for per-site skill references (track 0132).
//!
//! Checks the files under `sample_skill/sites/`: the strict-subset frontmatter, the body
//! headings, the caps, host/surface ownership, dates, URLs, the index table and a scoped
//! tripwire. It is the only consumer of those files. Nothing in `src/`, `extension/`,
//! `native-host/` or the build reads them, and this file is not a loader: it never answers a
//! host→skill question for the product.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_FILE_BYTES: usize = 12 * 1024;
const MAX_BODY_LINES: usize = 300;
const MAX_DESCRIPTION_CHARS: usize = 1024;
const MAX_COMPATIBILITY_CHARS: usize = 500;
const MAX_SURFACE_CHARS: usize = 32;
const TOP_LEVEL_KEYS: [&str; 5] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
];
const REQUIRED_META: [&str; 10] = [
    "hh-site-schema",
    "hh-site-revision",
    "hh-site-hosts",
    "hh-site-surface",
    "hh-site-last-verified",
    "hh-site-review-by",
    "hh-site-evidence",
    "hh-site-confidence",
    "hh-site-verified-url",
    "hh-site-author",
];
const OPTIONAL_META: [&str; 1] = ["hh-site-code-locus"];
const REQUIRED_H2: [&str; 9] = [
    "Use when",
    "What the page is",
    "Flows",
    "What to expect",
    "Pitfalls",
    "Extraction notes",
    "Safety",
    "Provenance",
    "Changelog",
];
const INDEX_HEADER: &str = "| Host(s) | Skill | Surface | Evidence | Last verified | Review by |";

/// A parsed `SKILL.md`: strict-subset frontmatter plus body lines (after the closing fence).
#[derive(Debug)]
struct Skill {
    top_keys: BTreeSet<String>,
    top: BTreeMap<String, String>,
    meta: BTreeMap<String, String>,
    body: Vec<String>,
    raw: String,
}

impl Skill {
    fn name(&self) -> &str {
        self.top.get("name").map_or("", String::as_str)
    }

    fn description(&self) -> &str {
        self.top.get("description").map_or("", String::as_str)
    }

    fn meta(&self, key: &str) -> &str {
        self.meta.get(key).map_or("", String::as_str)
    }

    fn hosts(&self) -> Vec<String> {
        self.meta("hh-site-hosts")
            .split(',')
            .map(|host| host.trim().to_string())
            .collect()
    }

    fn surface(&self) -> &str {
        self.meta("hh-site-surface")
    }
}

// ---------------------------------------------------------------------------
// Layer 1: dependency-free strict-subset parser.
// ---------------------------------------------------------------------------

fn parse_skill(text: &str) -> Result<Skill, Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    if lines.first() != Some(&"---") {
        return Err(vec![
            "frontmatter: first line must be the `---` fence".into(),
        ]);
    }
    let Some(close) = (1..lines.len()).find(|&i| lines[i] == "---") else {
        return Err(vec!["frontmatter: closing `---` fence not found".into()]);
    };

    let mut errors = Vec::new();
    let mut top_keys = BTreeSet::new();
    let mut top = BTreeMap::new();
    let mut meta = BTreeMap::new();
    let mut in_metadata = false;
    for (index, line) in lines[1..close].iter().enumerate() {
        let number = index + 2;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("  ") {
            if !in_metadata {
                errors.push(format!("line {number}: indented key outside `metadata:`"));
            } else if rest.starts_with([' ', '\t']) {
                errors.push(format!("line {number}: nested maps are not allowed"));
            } else {
                match parse_pair(rest) {
                    Ok((key, value)) => {
                        if meta.insert(key.clone(), value).is_some() {
                            errors.push(format!("line {number}: duplicate metadata key `{key}`"));
                        }
                    }
                    Err(message) => errors.push(format!("line {number}: {message}")),
                }
            }
            continue;
        }
        if line.starts_with([' ', '\t']) {
            errors.push(format!("line {number}: unsupported indentation"));
            continue;
        }
        in_metadata = false;
        if line.starts_with('-') {
            errors.push(format!("line {number}: lists are not allowed"));
            continue;
        }
        match parse_pair(line) {
            Ok((key, value)) => {
                if !top_keys.insert(key.clone()) {
                    errors.push(format!("line {number}: duplicate top-level key `{key}`"));
                    continue;
                }
                if key == "metadata" {
                    if value.is_empty() {
                        in_metadata = true;
                    } else {
                        errors.push(format!("line {number}: `metadata` must be a map"));
                    }
                }
                top.insert(key, value);
            }
            Err(message) => errors.push(format!("line {number}: {message}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Skill {
        top_keys,
        top,
        meta,
        body: lines[close + 1..]
            .iter()
            .map(|l| (*l).to_string())
            .collect(),
        raw: text.to_string(),
    })
}

/// Parses one `key: value` pair. Quotes are stripped once; folded, literal, list, map, anchor
/// and unquoted-colon values are refused.
fn parse_pair(line: &str) -> Result<(String, String), String> {
    let Some((key, raw_value)) = line.split_once(':') else {
        return Err(format!("expected `key: value`, got `{line}`"));
    };
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return Err(format!("bad key `{key}`"));
    }
    let value = raw_value.trim();
    if value.starts_with(['>', '|']) {
        return Err(format!(
            "`{key}`: folded (`>`) and literal (`|`) scalars are not allowed"
        ));
    }
    if value.starts_with(['[', '{']) {
        return Err(format!("`{key}`: lists and nested maps are not allowed"));
    }
    if value.starts_with(['&', '*']) {
        return Err(format!("`{key}`: anchors and aliases are not allowed"));
    }
    if let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
        if value.len() < 2 || !value.ends_with(quote) {
            return Err(format!("`{key}`: unterminated quoted scalar"));
        }
        return Ok((key.to_string(), value[1..value.len() - 1].to_string()));
    }
    if value.contains(':') {
        return Err(format!("`{key}`: a value containing `:` must be quoted"));
    }
    Ok((key.to_string(), value.to_string()))
}

// ---------------------------------------------------------------------------
// Layer 2: text-level rules for one skill.
// ---------------------------------------------------------------------------

fn check_skill(skill: &Skill, slug: &str) -> Vec<String> {
    check_skill_on(skill, slug, utc_today())
}

/// `today` is injected so the date rules are deterministic in tests.
fn check_skill_on(skill: &Skill, slug: &str, today: i64) -> Vec<String> {
    let mut errors = Vec::new();
    for key in &skill.top_keys {
        if key == "allowed-tools" {
            errors.push("`allowed-tools` is forbidden: a skill cannot pre-approve a tool".into());
        } else if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            errors.push(format!("unknown top-level key `{key}`"));
        }
    }
    for key in ["name", "description", "metadata"] {
        if !skill.top_keys.contains(key) {
            errors.push(format!("missing required key `{key}`"));
        }
    }
    let name = skill.name();
    if !is_slug(name, 64) {
        errors.push(format!(
            "name `{name}` must match ^[a-z0-9]+(-[a-z0-9]+)*$ (1-64 chars)"
        ));
    }
    if name != slug {
        errors.push(format!("name `{name}` must equal folder `{slug}`"));
    }
    check_description(skill, &mut errors);
    if skill
        .top
        .get("compatibility")
        .is_some_and(|compat| compat.chars().count() > MAX_COMPATIBILITY_CHARS)
    {
        errors.push("compatibility exceeds 500 characters".into());
    }
    if skill.raw.len() > MAX_FILE_BYTES {
        errors.push(format!(
            "file is {} bytes and exceeds 12 KiB",
            skill.raw.len()
        ));
    }
    if skill.body.len() > MAX_BODY_LINES {
        errors.push(format!(
            "body has {} lines and exceeds 300 lines",
            skill.body.len()
        ));
    }
    check_meta(skill, today, &mut errors);
    check_body(skill, &mut errors);
    for finding in tripwire_findings(&skill.raw) {
        errors.push(format!("tripwire: {finding}"));
    }
    errors
}

fn check_description(skill: &Skill, errors: &mut Vec<String>) {
    let description = skill.description();
    let count = description.chars().count();
    if count == 0 || count > MAX_DESCRIPTION_CHARS {
        errors.push(format!(
            "description must be 1-1024 characters (got {count})"
        ));
    }
    if !description.contains("Use when") {
        errors.push("description must say when to use it (`Use when`)".into());
    }
    let lower = description.to_ascii_lowercase();
    for host in skill.hosts() {
        if !lower.contains(&host) {
            errors.push(format!("description must name host `{host}`"));
        }
    }
}

fn check_meta(skill: &Skill, today: i64, errors: &mut Vec<String>) {
    for key in REQUIRED_META {
        if !skill.meta.contains_key(key) {
            errors.push(format!("missing metadata key `{key}`"));
        }
    }
    for key in skill.meta.keys() {
        let known = REQUIRED_META.contains(&key.as_str()) || OPTIONAL_META.contains(&key.as_str());
        if key.starts_with("hh-site-") && !known {
            errors.push(format!("unknown metadata key `{key}`"));
        }
    }
    if skill.meta("hh-site-schema") != "1" {
        errors.push("hh-site-schema must be \"1\"".into());
    }
    if !matches!(skill.meta("hh-site-revision").parse::<u32>(), Ok(r) if r >= 1) {
        errors.push("hh-site-revision must be an integer >= 1".into());
    }
    check_hosts(skill, errors);
    if !is_slug(skill.surface(), MAX_SURFACE_CHARS) {
        errors.push("hh-site-surface must be a slug of at most 32 characters".into());
    }
    check_dates(skill, today, errors);
    let evidence = skill.meta("hh-site-evidence");
    if !matches!(evidence, "live-observed" | "code-derived") {
        errors.push("hh-site-evidence must be live-observed or code-derived".into());
    }
    let confidence = skill.meta("hh-site-confidence");
    if !matches!(confidence, "high" | "medium" | "low") {
        errors.push("hh-site-confidence must be high, medium or low".into());
    }
    if confidence == "high" && evidence != "live-observed" {
        errors.push("hh-site-confidence high requires live-observed evidence".into());
    }
    check_verified_url(skill.meta("hh-site-verified-url"), &skill.hosts(), errors);
    let author = skill.meta("hh-site-author");
    let agent = author.strip_prefix("agent:").is_some_and(|harness| {
        !harness.is_empty()
            && harness
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    if author != "owner" && !agent {
        errors.push("hh-site-author must be `owner` or `agent:<harness>`".into());
    }
    if skill.meta.get("hh-site-code-locus").is_some_and(|locus| {
        locus
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b':' && w[1].is_ascii_digit())
    }) {
        errors.push("hh-site-code-locus must name symbols, not line numbers".into());
    }
}

fn check_hosts(skill: &Skill, errors: &mut Vec<String>) {
    let raw = skill.meta("hh-site-hosts");
    let hosts = skill.hosts();
    if raw.is_empty() || hosts.iter().any(String::is_empty) {
        errors.push("hh-site-hosts must list at least one host key".into());
    }
    if raw.contains(',') && raw != hosts.join(", ") {
        errors.push("hh-site-hosts must be separated by comma+space".into());
    }
    let mut seen = BTreeSet::new();
    for host in &hosts {
        match normalize_host(host) {
            Some(normal) if normal == *host => {}
            Some(normal) => errors.push(format!(
                "host entry `{host}` is not normalized (expected `{normal}`)"
            )),
            None => errors.push(format!(
                "host entry `{host}` is not normalized (not a bare host)"
            )),
        }
        if !seen.insert(host.clone()) {
            errors.push(format!("duplicate host entry `{host}`"));
        }
    }
}

fn check_dates(skill: &Skill, today: i64, errors: &mut Vec<String>) {
    let last = parse_date(skill.meta("hh-site-last-verified"));
    match last {
        None => {
            errors.push("hh-site-last-verified must be a real YYYY-MM-DD date (2020-2100)".into())
        }
        Some(day) if day > today + 1 => {
            errors.push("hh-site-last-verified is in the future".into())
        }
        Some(_) => {}
    }
    let review = parse_date(skill.meta("hh-site-review-by"));
    match (last, review) {
        (_, None) => {
            errors.push("hh-site-review-by must be a real YYYY-MM-DD date (2020-2100)".into())
        }
        (Some(last), Some(review)) if review <= last => {
            errors.push("hh-site-review-by must be after hh-site-last-verified".into());
        }
        (Some(last), Some(review)) if review - last > 365 => {
            errors.push(
                "hh-site-review-by must be at most 365 days after hh-site-last-verified".into(),
            );
        }
        _ => {}
    }
}

fn check_verified_url(url: &str, hosts: &[String], errors: &mut Vec<String>) {
    let Some(rest) = url.strip_prefix("https://") else {
        errors.push("hh-site-verified-url must be an https:// URL".into());
        return;
    };
    if url.contains(['?', '#', '@']) || url.contains(char::is_whitespace) {
        errors.push("hh-site-verified-url must have no query, fragment or userinfo".into());
    }
    let authority = rest.split('/').next().unwrap_or("");
    match normalize_host(authority) {
        Some(host) if hosts.contains(&host) => {}
        _ => errors.push("hh-site-verified-url host must be one of hh-site-hosts".into()),
    }
}

fn check_body(skill: &Skill, errors: &mut Vec<String>) {
    let (h1, h2) = scan_headings(&skill.body);
    if h1.len() == 1 {
        let label = skill
            .hosts()
            .first()
            .and_then(|host| host.split('.').next().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if !label.is_empty() && !h1[0].to_ascii_lowercase().contains(&label) {
            errors.push(format!("H1 must name the site (`{label}`)"));
        }
    } else {
        errors.push(format!("body needs exactly one H1 (found {})", h1.len()));
    }
    let in_order = h2.len() == REQUIRED_H2.len()
        && h2
            .iter()
            .zip(REQUIRED_H2)
            .all(|(found, want)| found.as_str() == want);
    if !in_order {
        errors.push(format!(
            "H2 headings must be exactly {REQUIRED_H2:?} in that order (found {h2:?})"
        ));
    }
    errors.extend(body_url_findings(&skill.body));
}

/// Reads `# ` and `## ` headings outside fenced code. `### ` subsections are ignored.
fn scan_headings(lines: &[String]) -> (Vec<String>, Vec<String>) {
    let mut in_fence = false;
    let mut h1 = Vec::new();
    let mut h2 = Vec::new();
    for line in lines {
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            h1.push(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("## ") {
            h2.push(rest.trim().to_string());
        }
    }
    (h1, h2)
}

/// Body URLs are `scheme://host/path` only: no query, fragment or userinfo.
fn body_url_findings(lines: &[String]) -> Vec<String> {
    let mut findings = Vec::new();
    for line in lines {
        for token in line.split_whitespace() {
            if !(token.contains("http://") || token.contains("https://")) {
                continue;
            }
            let url = token.trim_end_matches(|c: char| ".,;)`".contains(c));
            if url.contains(['?', '#', '@']) {
                findings.push(format!(
                    "URL `{url}` in the body carries a query, fragment or userinfo"
                ));
            }
        }
    }
    findings
}

fn is_slug(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

// ---------------------------------------------------------------------------
// Host normalization (spec §3.6): lowercase, one trailing dot, one leading www.
// ---------------------------------------------------------------------------

fn normalize_host(raw: &str) -> Option<String> {
    let mut host = raw.to_ascii_lowercase();
    if let Some(stripped) = host.strip_suffix('.') {
        host = stripped.to_string();
    }
    if let Some(stripped) = host.strip_prefix("www.") {
        host = stripped.to_string();
    }
    let refused =
        |c: char| matches!(c, ':' | '/' | '?' | '#' | '@' | '*' | '[' | ']') || c.is_whitespace();
    if host.is_empty() || host.chars().any(refused) {
        return None;
    }
    Some(host)
}

// ---------------------------------------------------------------------------
// Dates: dependency-free civil-day arithmetic (Howard Hinnant), UTC.
// ---------------------------------------------------------------------------

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Parses a strict `YYYY-MM-DD` real calendar date in 2020..=2100 to a civil day number.
fn parse_date(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = text[0..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..10].parse().ok()?;
    if !(2020..=2100).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn utc_today() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| (elapsed.as_secs() / 86_400) as i64)
}

// ---------------------------------------------------------------------------
// Tripwire (spec §3.12): anchored code/credential shapes, scoped to SKILL.md only.
// Hand-written matchers so no regex dependency is added.
// ---------------------------------------------------------------------------

type Probe = fn(&str) -> bool;

const TRIPWIRES: [(&str, Probe); 10] = [
    ("inline script tag `<script`", has_script_tag),
    ("DOM member access `document.<identifier>`", has_dom_member),
    ("CSS selection API `querySelector`", has_query_selector),
    ("scripted actuation call", has_scripted_call),
    ("credential assignment", has_credential_assignment),
    ("Authorization header value", has_authorization_header),
    ("Bearer token value", has_bearer_token),
    ("PEM private key", has_private_key),
    ("API key prefix", has_key_prefix),
    ("long hex blob (64+)", has_hex_blob),
];

const CREDENTIAL_KEYS: [&str; 12] = [
    "password",
    "passwd",
    "cookie",
    "session_id",
    "session-id",
    "sessionid",
    "api_key",
    "api-key",
    "apikey",
    "access_token",
    "access-token",
    "accesstoken",
];

const KEY_PREFIXES: [&str; 9] = [
    "sk",
    "pk",
    "ghp",
    "gho",
    "ghs",
    "github_pat",
    "xoxa",
    "xoxb",
    "xoxp",
];

fn tripwire_findings(text: &str) -> Vec<&'static str> {
    TRIPWIRES
        .iter()
        .filter(|(_, probe)| text.lines().any(*probe))
        .map(|(name, _)| *name)
        .collect()
}

fn at_word_start(line: &str, index: usize) -> bool {
    !line[..index]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_jwt_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

fn is_opaque_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

fn run_len(text: &str, class: fn(char) -> bool) -> usize {
    text.chars().take_while(|c| class(*c)).count()
}

fn has_script_tag(line: &str) -> bool {
    line.match_indices("<script").any(|(i, _)| {
        !line[i + "<script".len()..]
            .chars()
            .next()
            .is_some_and(is_word_char)
    })
}

fn has_dom_member(line: &str) -> bool {
    line.match_indices("document.").any(|(i, _)| {
        line[i + "document.".len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
    })
}

fn has_query_selector(line: &str) -> bool {
    line.contains("querySelector")
}

fn has_scripted_call(line: &str) -> bool {
    [".click", "addEventListener", "dispatchEvent"]
        .iter()
        .any(|&needle| {
            line.match_indices(needle)
                .any(|(i, _)| line[i + needle.len()..].trim_start().starts_with('('))
        })
}

/// True when `follows` accepts the text after a keyword that starts at a word boundary.
fn keyword_followed(line: &str, keys: &[&str], follows: fn(&str) -> bool) -> bool {
    keys.iter().any(|&key| {
        line.match_indices(key)
            .any(|(i, _)| at_word_start(line, i) && follows(&line[i + key.len()..]))
    })
}

/// `\s*[:=]\s*\S`
fn assigns_value(rest: &str) -> bool {
    let rest = rest.trim_start();
    match rest.strip_prefix(':').or_else(|| rest.strip_prefix('=')) {
        Some(after) => after
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace()),
        None => false,
    }
}

/// `\s*:\s*\S` (header form: colon only).
fn colon_value(rest: &str) -> bool {
    let rest = rest.trim_start();
    match rest.strip_prefix(':') {
        Some(after) => after
            .trim_start()
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace()),
        None => false,
    }
}

fn has_credential_assignment(line: &str) -> bool {
    keyword_followed(line, &CREDENTIAL_KEYS, assigns_value)
}

fn has_authorization_header(line: &str) -> bool {
    keyword_followed(line, &["Authorization"], colon_value)
}

fn has_bearer_token(line: &str) -> bool {
    line.match_indices("Bearer").any(|(i, _)| {
        if !at_word_start(line, i) {
            return false;
        }
        let rest = &line[i + "Bearer".len()..];
        let token = rest.trim_start();
        rest.len() > token.len() && bearer_shaped(token)
    })
}

/// `eyJ[A-Za-z0-9._-]{10,}` or `[A-Za-z0-9_-]{20,}\.[A-Za-z0-9._-]{10,}`.
fn bearer_shaped(token: &str) -> bool {
    if token
        .strip_prefix("eyJ")
        .is_some_and(|after| run_len(after, is_jwt_char) >= 10)
    {
        return true;
    }
    let run = run_len(token, is_opaque_char);
    run >= 20 && token[run..].starts_with('.') && run_len(&token[run + 1..], is_jwt_char) >= 10
}

fn has_private_key(line: &str) -> bool {
    line.match_indices("-----BEGIN ").any(|(i, _)| {
        // `-----BEGIN [A-Z ]*PRIVATE KEY-----`: only uppercase and spaces may precede the marker.
        let rest = &line[i + "-----BEGIN ".len()..];
        rest.find("PRIVATE KEY-----").is_some_and(|pos| {
            rest[..pos]
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == ' ')
        })
    })
}

fn has_key_prefix(line: &str) -> bool {
    KEY_PREFIXES.iter().any(|&prefix| {
        line.match_indices(prefix).any(|(i, _)| {
            if !at_word_start(line, i) {
                return false;
            }
            let rest = &line[i + prefix.len()..];
            match rest.strip_prefix(['-', '_']) {
                Some(body) => run_len(body, is_opaque_char) >= 16,
                None => false,
            }
        })
    })
}

fn has_hex_blob(line: &str) -> bool {
    line.split(|c: char| !is_word_char(c))
        .any(|token| token.len() >= 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// Layer 3: filesystem and index checks.
// ---------------------------------------------------------------------------

fn validate_sites_root(sites_root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let mut dirs: Vec<PathBuf> = match fs::read_dir(sites_root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(err) => return vec![format!("cannot read {}: {err}", sites_root.display())],
    };
    dirs.sort();

    let mut skills: Vec<(String, Skill)> = Vec::new();
    for dir in &dirs {
        let folder = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if folder.starts_with('.') {
            errors.push(format!(
                "`{folder}`: dot-directories are not allowed under sites/"
            ));
            continue;
        }
        let path = dir.join("SKILL.md");
        if !path.is_file() {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                errors.push(format!("`{folder}/SKILL.md`: {err}"));
                continue;
            }
        };
        match parse_skill(&text) {
            Ok(skill) => {
                errors.extend(
                    check_skill(&skill, &folder)
                        .into_iter()
                        .map(|m| format!("`{folder}/SKILL.md`: {m}")),
                );
                skills.push((folder, skill));
            }
            Err(found) => errors.extend(
                found
                    .into_iter()
                    .map(|m| format!("`{folder}/SKILL.md`: {m}")),
            ),
        }
    }
    if skills.is_empty() {
        errors.push(format!(
            "no shipped site skill under {} (expected <slug>/SKILL.md)",
            sites_root.display()
        ));
        return errors;
    }

    let mut owners: BTreeMap<(String, String), String> = BTreeMap::new();
    for (slug, skill) in &skills {
        for host in skill.hosts() {
            let key = (host.clone(), skill.surface().to_string());
            match owners.entry(key) {
                std::collections::btree_map::Entry::Occupied(prev) => errors.push(format!(
                    "duplicate (host, surface) pair ({host}, {}) claimed by `{}` and `{slug}`",
                    skill.surface(),
                    prev.get()
                )),
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(slug.clone());
                }
            }
        }
    }

    check_index(sites_root, &skills, &mut errors);
    errors
}

fn check_index(sites_root: &Path, skills: &[(String, Skill)], errors: &mut Vec<String>) {
    let index = match fs::read_to_string(sites_root.join("README.md")) {
        Ok(text) => text,
        Err(_) => {
            errors.push("index `README.md` is missing from sites/".into());
            return;
        }
    };
    let Some(rows) = index_rows(&index) else {
        errors.push(format!("index has no table with header `{INDEX_HEADER}`"));
        return;
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut previous: Option<(String, String)> = None;
    for cells in &rows {
        if cells.len() != 6 {
            errors.push(format!(
                "index row has {} cells, expected 6: {}",
                cells.len(),
                cells.join(" | ")
            ));
            continue;
        }
        let (hosts, slug, surface, evidence, last, review) = (
            &cells[0], &cells[1], &cells[2], &cells[3], &cells[4], &cells[5],
        );
        let Some((_, skill)) = skills.iter().find(|(folder, _)| folder == slug) else {
            errors.push(format!("index row `{slug}` has no skill folder"));
            continue;
        };
        if !seen.insert(slug.clone()) {
            errors.push(format!("index lists `{slug}` more than once"));
        }
        let checks = [
            ("hosts", *hosts == skill.hosts().join(", ")),
            ("surface", surface == skill.surface()),
            ("evidence", evidence == skill.meta("hh-site-evidence")),
            ("last verified", last == skill.meta("hh-site-last-verified")),
            ("review by", review == skill.meta("hh-site-review-by")),
        ];
        for (field, same) in checks {
            if !same {
                errors.push(format!(
                    "index row `{slug}`: {field} differs from the skill metadata"
                ));
            }
        }
        let key = (
            hosts.split(',').next().unwrap_or("").trim().to_string(),
            surface.clone(),
        );
        if previous.as_ref().is_some_and(|prev| key < *prev) {
            errors.push("index rows must be sorted by first host, then surface".into());
        }
        previous = Some(key);
    }
    for (folder, _) in skills {
        if !seen.contains(folder) {
            errors.push(format!("no index row for skill `{folder}`"));
        }
    }
}

/// Returns the data rows of the one index table whose header is `INDEX_HEADER`.
fn index_rows(index: &str) -> Option<Vec<Vec<String>>> {
    let mut lines = index.lines();
    lines.by_ref().find(|line| line.trim() == INDEX_HEADER)?;
    let mut rows = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<String> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().trim_matches('`').to_string())
            .collect();
        let separator = cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'));
        if !separator {
            rows.push(cells);
        }
    }
    Some(rows)
}

// ---------------------------------------------------------------------------
// Paths and shared helpers for the tests below.
// ---------------------------------------------------------------------------

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn shipped_sites() -> PathBuf {
    manifest().join("sample_skill").join("sites")
}

fn fixture_sites(case: &str) -> PathBuf {
    manifest()
        .join("tests")
        .join("fixtures")
        .join("site_skills")
        .join(case)
        .join("sites")
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn fixed_today() -> i64 {
    days_from_civil(2026, 11, 10)
}

/// Parse + check one inline text, returning every problem (parse problems included).
fn errors_for(text: &str, slug: &str) -> Vec<String> {
    match parse_skill(text) {
        Ok(skill) => check_skill_on(&skill, slug, fixed_today()),
        Err(errors) => errors,
    }
}

fn mutate(base: &str, from: &str, to: &str) -> String {
    assert!(base.contains(from), "mutation anchor missing: {from:?}");
    base.replacen(from, to, 1)
}

fn swap_headings(base: &str, a: &str, b: &str) -> String {
    base.replace(&format!("## {a}"), "@@SWAP@@")
        .replace(&format!("## {b}"), &format!("## {a}"))
        .replace("@@SWAP@@", &format!("## {b}"))
}

fn good_text() -> String {
    read(fixture_sites("good").join("good-site").join("SKILL.md"))
}

/// Shipped skill folders (`<slug>/SKILL.md`), sorted, as (slug, text).
fn shipped_skill_files() -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(shipped_sites()) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").is_file())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|dir| {
            let slug = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (slug, read(dir.join("SKILL.md")))
        })
        .collect()
}

fn require_shipped() {
    let count = shipped_skill_files().len();
    assert!(
        count >= 1,
        "no shipped site skill under {} (expected <slug>/SKILL.md)",
        shipped_sites().display()
    );
}

fn shipped_errors() -> Vec<String> {
    require_shipped();
    validate_sites_root(&shipped_sites())
}

fn parse_shipped() -> Vec<(String, Skill)> {
    shipped_skill_files()
        .into_iter()
        .filter_map(|(slug, text)| parse_skill(&text).ok().map(|skill| (slug, skill)))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[test]
fn fixture_good_parses() {
    let root = fixture_sites("good");
    let errors = validate_sites_root(&root);
    assert!(errors.is_empty(), "good fixture must pass: {errors:#?}");
    let skill = parse_skill(&good_text()).expect("good fixture parses");
    assert_eq!(skill.name(), "good-site");
    assert!(check_skill_on(&skill, "good-site", fixed_today()).is_empty());
}

#[test]
fn negative_matrix_each_fails_once() {
    let good = good_text();
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "bad name",
            mutate(&good, "name: good-site", "name: Good_Site"),
            "must match",
        ),
        (
            "missing heading",
            mutate(&good, "## Pitfalls\n", ""),
            "H2 headings must be exactly",
        ),
        (
            "H2 order drift",
            swap_headings(&good, "What the page is", "What to expect"),
            "H2 headings must be exactly",
        ),
        (
            "oversize file",
            format!("{good}{}", "x".repeat(MAX_FILE_BYTES)),
            "exceeds 12 KiB",
        ),
        (
            "oversize body",
            format!("{good}{}", "- filler\n".repeat(300)),
            "exceeds 300 lines",
        ),
        (
            "unnormalized host entry",
            mutate(
                &good,
                "hh-site-hosts: \"example.test\"",
                "hh-site-hosts: \"WWW.example.test\"",
            ),
            "not normalized",
        ),
        (
            "future date",
            mutate(
                &good,
                "hh-site-last-verified: \"2026-11-10\"",
                "hh-site-last-verified: \"2099-01-01\"",
            ),
            "in the future",
        ),
        (
            "review_by out of range",
            mutate(
                &good,
                "hh-site-review-by: \"2027-05-09\"",
                "hh-site-review-by: \"2028-06-01\"",
            ),
            "at most 365 days",
        ),
        (
            "selector tripwire",
            mutate(
                &good,
                "## Flows\n",
                "## Flows\nCall document.getElementById on the grid.\n",
            ),
            "tripwire",
        ),
        (
            "secret tripwire",
            mutate(&good, "## Pitfalls\n", "## Pitfalls\napi_key: abc123456\n"),
            "tripwire",
        ),
        (
            "allowed-tools",
            mutate(&good, "metadata:\n", "allowed-tools: \"Bash\"\nmetadata:\n"),
            "allowed-tools",
        ),
        (
            "unknown top-level key",
            mutate(&good, "metadata:\n", "author: someone\nmetadata:\n"),
            "unknown top-level key",
        ),
        (
            "bad verified-url (query)",
            mutate(
                &good,
                "\"https://example.test/results/\"",
                "\"https://example.test/results/?q=1\"",
            ),
            "verified-url",
        ),
        (
            "bad verified-url (http)",
            mutate(
                &good,
                "\"https://example.test/results/\"",
                "\"http://example.test/results/\"",
            ),
            "verified-url",
        ),
        (
            "confidence/evidence mismatch",
            mutate(
                &good,
                "hh-site-confidence: \"medium\"",
                "hh-site-confidence: \"high\"",
            ),
            "live-observed evidence",
        ),
        (
            "folded description",
            mutate(&good, "description: \"Example", "description: >\n  Example"),
            "folded",
        ),
    ];
    for (label, text, token) in cases {
        let errors = errors_for(&text, "good-site");
        assert!(
            errors.iter().any(|e| e.contains(token)),
            "{label}: expected an error containing {token:?}, got {errors:#?}"
        );
    }

    let trees = [
        ("bad-folder-name", "must equal folder"),
        ("host-surface-duplicate", "duplicate (host, surface)"),
        ("index-drift", "no index row for skill"),
        ("empty-root", "no shipped site skill"),
    ];
    for (case, token) in trees {
        let errors = validate_sites_root(&fixture_sites(case));
        assert!(
            errors.iter().any(|e| e.contains(token)),
            "{case}: expected {token:?}, got {errors:#?}"
        );
    }
}

#[test]
fn parser_handles_crlf_and_quotes() {
    let good = good_text();
    let crlf = format!("\u{feff}{}", good.replace('\n', "\r\n"));
    let skill = parse_skill(&crlf).expect("BOM + CRLF parses");
    assert!(check_skill_on(&skill, "good-site", fixed_today()).is_empty());

    let single = mutate(&good, "name: good-site", "name: 'good-site'");
    let skill = parse_skill(&single).expect("single-quoted scalar parses");
    assert_eq!(skill.name(), "good-site");

    let folded = mutate(&good, "description: \"Example", "description: >\n  Example");
    let errors = parse_skill(&folded).expect_err("folded description is refused");
    assert!(errors.iter().any(|e| e.contains("folded")), "{errors:#?}");

    let unquoted = mutate(
        &good,
        "hh-site-verified-url: \"https://example.test/results/\"",
        "hh-site-verified-url: https://example.test/results/",
    );
    let errors = parse_skill(&unquoted).expect_err("unquoted colon value is refused");
    assert!(
        errors.iter().any(|e| e.contains("must be quoted")),
        "{errors:#?}"
    );
}

#[test]
fn heading_h3_subsections_are_allowed() {
    let errors = validate_sites_root(&fixture_sites("h3-subsections-ok"));
    assert!(
        errors.is_empty(),
        "H3 subsections and fenced `##` must pass: {errors:#?}"
    );
    let text = read(
        fixture_sites("h3-subsections-ok")
            .join("h3-site")
            .join("SKILL.md"),
    );
    assert!(
        text.contains("### Card anatomy"),
        "fixture must carry an H3 subsection"
    );
    assert!(
        text.contains("## Not a heading"),
        "fixture must carry a fenced `##` line"
    );
}

#[test]
fn tripwire_allows_required_prose() {
    let prose = [
        "- Never record passwords or cookies.",
        "Read the document. In this page the results are listed.",
        "Provenance: code read at commit 0ad4596c832b869e66a7df2a78b817de7cb2459b.",
        "Symbols: cardDealer, infer_card_kind, parse_empty_state.",
    ];
    for line in prose {
        assert!(
            tripwire_findings(line).is_empty(),
            "required prose tripped: {line}"
        );
    }
}

#[test]
fn tripwire_catches_forbidden_constructs() {
    let forbidden = [
        "<script>alert(1)</script>",
        "Call document.getElementById on the grid.",
        "Use querySelector to find the card.",
        "Then btn.click() the control.",
        "addEventListener ('click', handler)",
        "password: hunter2",
        "cookie=abc",
        "Authorization: Basic dXNlcjpwYXNz",
        "Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig",
        "-----BEGIN RSA PRIVATE KEY-----",
        "token sk-abcdefghijklmnop1234",
        "ghp_abcdefghijklmnopqrstu",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ];
    for line in forbidden {
        assert!(
            !tripwire_findings(line).is_empty(),
            "forbidden shape passed: {line}"
        );
    }
}

#[test]
fn second_surface_on_same_host_is_legal() {
    let errors = validate_sites_root(&fixture_sites("second-surface"));
    assert!(
        errors.is_empty(),
        "second surface on one host must pass: {errors:#?}"
    );
    let dup = validate_sites_root(&fixture_sites("host-surface-duplicate"));
    assert!(
        dup.iter().any(|e| e.contains("duplicate (host, surface)")),
        "same host and surface must fail: {dup:#?}"
    );
}

#[test]
fn host_normalization_examples_hold() {
    let cases: [(&str, Option<&str>); 12] = [
        ("WWW.Cars.com.", Some("cars.com")),
        ("www.cars.com", Some("cars.com")),
        ("cars.com.", Some("cars.com")),
        ("m.cars.com", Some("m.cars.com")),
        ("evil.cars.com", Some("evil.cars.com")),
        ("cars.com.evil.test", Some("cars.com.evil.test")),
        ("www.cars.com.evil.test", Some("cars.com.evil.test")),
        ("cars.com:443", None),
        ("https://cars.com/", None),
        ("cars.com/", None),
        ("*.cars.com", None),
        ("user@cars.com", None),
    ];
    for (raw, want) in cases {
        assert_eq!(normalize_host(raw).as_deref(), want, "normalize({raw:?})");
    }
    let owned = normalize_host("cars.com").expect("owned host normalizes");
    for lookalike in [
        "evil.cars.com",
        "cars.com.evil.test",
        "www.cars.com.evil.test",
    ] {
        assert_ne!(
            normalize_host(lookalike).as_deref(),
            Some(owned.as_str()),
            "{lookalike}"
        );
    }
    for entry in [
        "WWW.Cars.com.",
        "www.cars.com",
        "cars.com.",
        "cars.com:443",
        "https://cars.com/",
    ] {
        assert_ne!(
            normalize_host(entry).as_deref(),
            Some(entry),
            "entry {entry} must not be accepted as normal"
        );
    }
}

#[test]
fn at_least_one_shipped_skill_exists() {
    require_shipped();
}

#[test]
fn shipped_skills_are_conformant() {
    let errors = shipped_errors();
    assert!(
        errors.is_empty(),
        "shipped site skills must conform: {errors:#?}"
    );
}

#[test]
fn site_skill_host_surface_pairs_are_normalized_and_unique() {
    let errors = shipped_errors();
    assert!(
        errors
            .iter()
            .all(|e| !e.contains("duplicate (host, surface)") && !e.contains("not normalized")),
        "{errors:#?}"
    );
}

#[test]
fn site_skill_index_matches_folders() {
    let errors = shipped_errors();
    let index: Vec<&String> = errors.iter().filter(|e| e.contains("index")).collect();
    assert!(
        index.is_empty(),
        "index must agree with the folders: {index:#?}"
    );
}

#[test]
fn site_skill_dates_are_ordered_and_not_future() {
    require_shipped();
    let today = utc_today();
    for (slug, skill) in parse_shipped() {
        let last = parse_date(skill.meta("hh-site-last-verified"))
            .unwrap_or_else(|| panic!("{slug}: last-verified is not a date"));
        assert!(last <= today + 1, "{slug}: last-verified is in the future");
        let review = parse_date(skill.meta("hh-site-review-by"))
            .unwrap_or_else(|| panic!("{slug}: review-by is not a date"));
        assert!(
            review > last && review - last <= 365,
            "{slug}: review-by out of range"
        );
    }
}

#[test]
fn site_skill_provenance_is_redacted() {
    require_shipped();
    for (slug, skill) in parse_shipped() {
        let findings = body_url_findings(&skill.body);
        assert!(findings.is_empty(), "{slug}: {findings:#?}");
    }
    let errors = shipped_errors();
    assert!(
        errors.iter().all(|e| !e.contains("verified-url")),
        "verified-url must be host+path only: {errors:#?}"
    );
}

#[test]
fn site_skill_staleness_rule_documented() {
    let index = read(shipped_sites().join("README.md")).to_ascii_lowercase();
    for phrase in [
        "advisory-only",
        "live observation wins",
        "hh-site-review-by",
    ] {
        assert!(
            index.contains(phrase),
            "sites/README.md must state the staleness rule: missing {phrase:?}"
        );
    }
}

#[test]
fn site_skill_cannot_preapprove_tools() {
    require_shipped();
    for (slug, skill) in parse_shipped() {
        assert!(
            !skill.top_keys.contains("allowed-tools"),
            "{slug} pre-approves tools"
        );
    }
    let errors = shipped_errors();
    assert!(
        errors.iter().all(|e| !e.contains("allowed-tools")),
        "{errors:#?}"
    );
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn site_skills_change_no_runtime() {
    let root = manifest();
    let mut files = Vec::new();
    for dir in ["src", "extension", "native-host"] {
        collect_files(&root.join(dir), &mut files);
    }
    files.push(root.join("Cargo.toml"));
    files.push(root.join("build.rs"));
    let mut hits = Vec::new();
    for file in files {
        let Ok(bytes) = fs::read(&file) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("sample_skill") || text.contains("sites/") {
            hits.push(file.display().to_string());
        }
    }
    assert!(
        hits.is_empty(),
        "runtime paths must not reference the site skills: {hits:#?}"
    );
}

#[test]
fn product_tree_has_no_governance_docs() {
    let output = Command::new("git")
        .arg("ls-files")
        .current_dir(manifest())
        .output()
        .expect("run `git ls-files` (git is required for this lock)");
    assert!(output.status.success(), "git ls-files failed");
    let listed = String::from_utf8_lossy(&output.stdout);
    assert!(!listed.trim().is_empty(), "git ls-files returned no files");
    let governance: Vec<&str> = listed
        .lines()
        .filter(|path| {
            path.starts_with("conductor/")
                || *path == "planner.md"
                || path.ends_with("-review.md")
                || path.ends_with("/review.md")
                || *path == "review.md"
        })
        .collect();
    assert!(
        governance.is_empty(),
        "governance docs must not be tracked in hands/: {governance:#?}"
    );
}

fn cars_skill_text() -> String {
    read(shipped_sites().join("cars-com").join("SKILL.md"))
}

fn body_of(text: &str) -> String {
    let skill = parse_skill(text).expect("cars-com skill parses");
    skill.body.join("\n").to_ascii_lowercase()
}

#[test]
fn cars_skill_covers_required_knowledge() {
    let body = body_of(&cars_skill_text());
    let tokens = [
        "results",
        "detail",
        "cards_walked",
        "cards_total",
        "local",
        "ship",
        "recommended",
        "contact dealer",
        "one-word",
        "/mo",
        "est.",
        "radius",
        "empty state",
        "fence",
    ];
    let missing: Vec<&str> = tokens.into_iter().filter(|t| !body.contains(t)).collect();
    assert!(
        missing.is_empty(),
        "cars-com body is missing required knowledge: {missing:?}"
    );
}

#[test]
fn cars_skill_claims_no_live_facts() {
    let text = cars_skill_text();
    let skill = parse_skill(&text).expect("cars-com skill parses");
    assert_eq!(
        skill.meta("hh-site-evidence"),
        "code-derived",
        "no live observation was made"
    );
    assert_eq!(
        skill.meta("hh-site-confidence"),
        "medium",
        "code-derived caps confidence at medium"
    );
    assert!(
        body_of(&text).contains("not verified"),
        "cars-com must state what is not verified"
    );
}

#[test]
fn site_skill_docs_state_the_trust_rule() {
    let readme = read(manifest().join("README.md"));
    assert!(
        readme.contains("sample_skill/sites/README.md"),
        "README.md must link the sites index"
    );
    assert!(
        readme.contains("untrusted reference data"),
        "README.md must state the trust rule"
    );
    let contract = read(manifest().join("docs").join("AGENT-CONTRACT.md"));
    assert!(
        contract.contains("untrusted reference data"),
        "AGENT-CONTRACT must state the trust rule"
    );
    assert!(
        contract.contains("cannot authorize"),
        "AGENT-CONTRACT must say skills cannot authorize"
    );
}

#[test]
fn sample_skill_readme_names_sites() {
    let readme = read(manifest().join("sample_skill").join("README.md"));
    assert!(
        readme.contains("sites/"),
        "sample_skill/README.md must name the sites/ copy option"
    );
}
