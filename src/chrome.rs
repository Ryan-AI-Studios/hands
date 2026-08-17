//! MCP/CLI client for the Chrome host (fixture or named pipe).

use serde::Deserialize;
use std::fs;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::core::PCWSTR;

use crate::error::HandsError;
use crate::extract::{Card, Detail, Element, take_chars};
use crate::native_host::{CLIENT_TIMEOUT_MS, client_timeout, exchange_pipe_deadline, pipe_name};
use crate::space::Rect;

pub const SNAPSHOT_ENV: &str = "HANDS_CHROME_SNAPSHOT";
pub const PIPE_ENV: &str = "HANDS_CHROME_PIPE";
pub const CARD_TITLE_CAP: usize = 80;
pub const CARD_PRICE_CAP: usize = 24;
pub const CARD_HREF_CAP: usize = 200;
pub const CARD_CAP: usize = 8;

#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub struct ChromeMap {
    pub url: Option<String>,
    pub title: String,
    pub main_text: String,
    pub elements: Vec<Element>,
    pub cards: Vec<Card>,
}

#[derive(Debug, Clone)]
pub struct ChromeHit {
    pub id: String,
    pub name: String,
    pub role: String,
    pub rect: Rect,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChromeMetrics {
    #[serde(rename = "screenX")]
    pub screen_x: f64,
    #[serde(rename = "screenY")]
    pub screen_y: f64,
    #[serde(rename = "outerWidth")]
    pub outer_width: f64,
    #[serde(rename = "outerHeight")]
    pub outer_height: f64,
    #[serde(rename = "innerWidth")]
    pub inner_width: f64,
    #[serde(rename = "innerHeight")]
    pub inner_height: f64,
    #[serde(rename = "devicePixelRatio")]
    pub device_pixel_ratio: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CssRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct RawNode {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "rectCss")]
    rect_css: CssRect,
    #[serde(default, rename = "href")]
    _href: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCard {
    #[serde(default)]
    title: String,
    #[serde(default)]
    price: String,
    #[serde(default)]
    href: String,
    #[serde(rename = "rectCss")]
    rect_css: CssRect,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSnapshot {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    main_text: Option<String>,
    #[serde(default)]
    elements: Vec<RawNode>,
    #[serde(default)]
    cards: Vec<RawCard>,
    metrics: ChromeMetrics,
}

pub fn css_rect_to_physical(metrics: &ChromeMetrics, rect: &CssRect) -> Option<Rect> {
    let dpr = metrics.device_pixel_ratio;
    if !dpr.is_finite() || dpr <= 0.0 {
        return None;
    }
    if !rect.width.is_finite() || !rect.height.is_finite() {
        return None;
    }
    if !metrics.screen_x.is_finite()
        || !metrics.screen_y.is_finite()
        || !metrics.outer_width.is_finite()
        || !metrics.outer_height.is_finite()
        || !metrics.inner_width.is_finite()
        || !metrics.inner_height.is_finite()
    {
        return None;
    }
    let chrome_ui_y = (metrics.outer_height - metrics.inner_height).max(0.0);
    let chrome_ui_x = (metrics.outer_width - metrics.inner_width).max(0.0) / 2.0;
    let css_x = metrics.screen_x + chrome_ui_x + rect.left;
    let css_y = metrics.screen_y + chrome_ui_y + rect.top;
    let x = round_i32(css_x * dpr)?;
    let y = round_i32(css_y * dpr)?;
    let w = round_i32(rect.width * dpr)?;
    let h = round_i32(rect.height * dpr)?;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Rect { x, y, w, h })
}

fn round_i32(v: f64) -> Option<i32> {
    if !v.is_finite() {
        return None;
    }
    let r = v.round();
    if r < f64::from(i32::MIN) || r > f64::from(i32::MAX) {
        return None;
    }
    Some(r as i32)
}

pub fn try_snapshot(detail: Detail) -> Option<ChromeMap> {
    match snapshot_env() {
        Some(path) => load_fixture(&path).ok(),
        None => pipe_snapshot(detail).ok(),
    }
}

pub fn try_resolve(id: &str) -> Result<ChromeHit, HandsError> {
    let map = match snapshot_env() {
        Some(path) => load_fixture(&path)?,
        None => pipe_resolve(id)?,
    };
    let el = map.elements.iter().find(|e| e.id == id).ok_or_else(|| {
        HandsError::Chrome(format!(
            "Chrome element {id} not found in host/fixture snapshot"
        ))
    })?;
    Ok(ChromeHit {
        id: el.id.clone(),
        name: el.text.clone().unwrap_or_default(),
        role: el.role.clone(),
        rect: el.rect,
    })
}

fn snapshot_env() -> Option<String> {
    std::env::var(SNAPSHOT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn load_fixture(path: &str) -> Result<ChromeMap, HandsError> {
    let bytes = fs::read(path).map_err(|_| {
        HandsError::Chrome(format!(
            "Chrome snapshot fixture is missing or unreadable ({path}); cannot resolve chr: (host/fixture)"
        ))
    })?;
    parse_snapshot_bytes(&bytes).map_err(|_| {
        HandsError::Chrome(format!(
            "Chrome snapshot fixture is invalid ({path}); cannot resolve chr: (host/fixture)"
        ))
    })
}

fn parse_snapshot_bytes(bytes: &[u8]) -> Result<ChromeMap, HandsError> {
    let raw: RawSnapshot = serde_json::from_slice(bytes)
        .map_err(|err| HandsError::Chrome(format!("Chrome snapshot JSON: {err}")))?;
    Ok(map_from_raw(raw))
}

fn map_from_raw(raw: RawSnapshot) -> ChromeMap {
    let mut elements = Vec::new();
    for (i, node) in raw.elements.into_iter().enumerate() {
        let Some(rect) = css_rect_to_physical(&raw.metrics, &node.rect_css) else {
            continue;
        };
        let id = node
            .id
            .as_deref()
            .and_then(canonical_chr)
            .unwrap_or_else(|| format!("chr:{i}"));
        elements.push(Element {
            id,
            role: node.role.unwrap_or_else(|| "Other".into()),
            text: node.text,
            rect,
        });
    }
    let mut cards = Vec::new();
    for card in raw.cards {
        if cards.len() >= CARD_CAP {
            break;
        }
        if !href_ok(&card.href) {
            continue;
        }
        let Some(rect) = css_rect_to_physical(&raw.metrics, &card.rect_css) else {
            continue;
        };
        cards.push(Card {
            title: take_chars(&card.title, CARD_TITLE_CAP),
            price: take_chars(&card.price, CARD_PRICE_CAP),
            href: take_chars(&card.href, CARD_HREF_CAP),
            rect,
        });
    }
    ChromeMap {
        url: raw.url.filter(|s| !s.trim().is_empty()),
        title: raw.title.unwrap_or_default(),
        main_text: raw.main_text.unwrap_or_default(),
        elements,
        cards,
    }
}

fn canonical_chr(id: &str) -> Option<String> {
    let rest = id.strip_prefix("chr:")?;
    if rest.is_empty()
        || rest.starts_with('+')
        || rest.starts_with('-')
        || (rest.len() > 1 && rest.starts_with('0'))
        || !rest.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    rest.parse::<u32>().ok().map(|n| format!("chr:{n}"))
}

fn href_ok(href: &str) -> bool {
    let t = href.trim();
    if t.is_empty() {
        return false;
    }
    !t.to_ascii_lowercase().starts_with("javascript:")
}

fn pipe_snapshot(detail: Detail) -> Result<ChromeMap, HandsError> {
    let mut req = serde_json::json!({"op": "snapshot"});
    if detail == Detail::Dom {
        req["detail"] = serde_json::Value::String("dom".into());
    }
    let reply = pipe_request(&req)?;
    parse_host_reply(&reply)
}

fn pipe_resolve(id: &str) -> Result<ChromeMap, HandsError> {
    let req = serde_json::json!({"op": "resolve", "id": id});
    let reply = pipe_request(&req).map_err(|err| {
        HandsError::Chrome(format!(
            "Chrome host is not connected (pipe); cannot resolve {id}: {err}"
        ))
    })?;
    if reply.get("error").is_some()
        && reply.get("elements").is_none()
        && reply.get("rectCss").is_none()
    {
        return Err(HandsError::Chrome(format!(
            "Chrome host/fixture did not resolve {id}"
        )));
    }
    if reply.get("rectCss").is_some() {
        let raw: RawNode = serde_json::from_value(reply.clone())
            .map_err(|err| HandsError::Chrome(format!("Chrome resolve payload for {id}: {err}")))?;
        let metrics: ChromeMetrics =
            serde_json::from_value(reply.get("metrics").cloned().ok_or_else(|| {
                HandsError::Chrome(format!("Chrome resolve {id} missing metrics"))
            })?)
            .map_err(|err| HandsError::Chrome(format!("Chrome resolve metrics: {err}")))?;
        return Ok(map_from_raw(RawSnapshot {
            url: None,
            title: None,
            main_text: None,
            elements: vec![raw],
            cards: Vec::new(),
            metrics,
        }));
    }
    parse_host_reply(&reply)
}

fn parse_host_reply(reply: &serde_json::Value) -> Result<ChromeMap, HandsError> {
    if reply.get("error").is_some() && reply.get("elements").is_none() {
        return Err(HandsError::Chrome(format!(
            "Chrome host error: {}",
            reply["error"]
        )));
    }
    let raw: RawSnapshot = serde_json::from_value(reply.clone())
        .map_err(|err| HandsError::Chrome(format!("Chrome snapshot payload: {err}")))?;
    Ok(map_from_raw(raw))
}

fn pipe_request(req: &serde_json::Value) -> Result<serde_json::Value, HandsError> {
    let deadline = Instant::now() + Duration::from_millis(u64::from(CLIENT_TIMEOUT_MS));
    let name = pipe_name();
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let wait_ms = remaining_client_ms(deadline)?;
    let ready = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), wait_ms) };
    if !ready.as_bool() {
        return Err(HandsError::Chrome(format!(
            "Chrome host is not connected (no pipe {name} within {CLIENT_TIMEOUT_MS} ms); chr: unavailable"
        )));
    }
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|err| {
        HandsError::Chrome(format!(
            "Chrome host pipe connect failed ({name}): {err}; chr: unavailable"
        ))
    })?;
    let result = exchange_pipe_deadline(handle, req, deadline);
    let _ = unsafe { CloseHandle(handle) };
    result
}

fn remaining_client_ms(deadline: Instant) -> Result<u32, HandsError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(client_timeout());
    }
    Ok(u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX))
}

#[cfg(test)]
pub(crate) struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev_snap: Option<std::ffi::OsString>,
    prev_pipe: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvGuard {
    pub fn lock() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            prev_snap: std::env::var_os(SNAPSHOT_ENV),
            prev_pipe: std::env::var_os(PIPE_ENV),
            _lock: lock,
        }
    }

    pub fn set_snapshot(&self, path: Option<&std::path::Path>) {
        match path {
            Some(p) => unsafe { std::env::set_var(SNAPSHOT_ENV, p) },
            None => unsafe { std::env::remove_var(SNAPSHOT_ENV) },
        }
    }

    pub fn set_pipe(&self, name: Option<&str>) {
        match name {
            Some(n) => unsafe { std::env::set_var(PIPE_ENV, n) },
            None => unsafe { std::env::remove_var(PIPE_ENV) },
        }
    }

    pub fn fixture_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/chrome-snapshot.json")
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev_snap {
            Some(v) => unsafe { std::env::set_var(SNAPSHOT_ENV, v) },
            None => unsafe { std::env::remove_var(SNAPSHOT_ENV) },
        }
        match &self.prev_pipe {
            Some(v) => unsafe { std::env::set_var(PIPE_ENV, v) },
            None => unsafe { std::env::remove_var(PIPE_ENV) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_metrics() -> ChromeMetrics {
        ChromeMetrics {
            screen_x: 100.0,
            screen_y: 50.0,
            outer_width: 1280.0,
            outer_height: 800.0,
            inner_width: 1280.0,
            inner_height: 720.0,
            device_pixel_ratio: 1.0,
        }
    }

    fn fixture_rect() -> CssRect {
        CssRect {
            left: 10.0,
            top: 20.0,
            width: 200.0,
            height: 32.0,
        }
    }

    #[test]
    fn geometry_dpr1_toolbar_80() {
        let rect = css_rect_to_physical(&fixture_metrics(), &fixture_rect()).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 110,
                y: 150,
                w: 200,
                h: 32
            }
        );
        assert_eq!(rect.center(), (210, 166));
    }

    #[test]
    fn geometry_dpr_1_25() {
        let mut metrics = fixture_metrics();
        metrics.device_pixel_ratio = 1.25;
        let rect = css_rect_to_physical(&metrics, &fixture_rect()).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 138,
                y: 188,
                w: 250,
                h: 40
            }
        );
    }

    #[test]
    fn geometry_skips_bad_dpr_and_zero_size() {
        let mut metrics = fixture_metrics();
        metrics.device_pixel_ratio = 0.0;
        assert!(css_rect_to_physical(&metrics, &fixture_rect()).is_none());
        metrics.device_pixel_ratio = f64::NAN;
        assert!(css_rect_to_physical(&metrics, &fixture_rect()).is_none());
        let mut rect = fixture_rect();
        rect.width = 0.0;
        assert!(css_rect_to_physical(&fixture_metrics(), &rect).is_none());
    }

    #[test]
    fn fixture_snapshot_sets_chr_and_url() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(&EnvGuard::fixture_path()));
        g.set_pipe(None);
        let map = try_snapshot(Detail::Default).expect("fixture");
        assert_eq!(map.url.as_deref(), Some("https://cars.com/search"));
        assert_eq!(map.title, "Cars.com");
        assert_eq!(map.elements[0].id, "chr:0");
        assert_eq!(
            map.elements[0].rect,
            Rect {
                x: 110,
                y: 150,
                w: 200,
                h: 32
            }
        );
        assert!(map.elements.iter().any(|e| e.text.is_none()));
        assert_eq!(map.cards.len(), 1);
        assert_eq!(map.cards[0].price, "$12,345");
        assert!(!map.main_text.to_ascii_lowercase().contains("hunter"));
    }

    #[test]
    fn missing_or_invalid_fixture_is_absent_for_observe() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(std::path::Path::new(
            r"C:\dev\Helping-Hands\hands\tests\fixtures\missing-chrome.json",
        )));
        assert!(try_snapshot(Detail::Default).is_none());
        let bad = std::env::temp_dir().join("hands-chrome-bad.json");
        fs::write(&bad, "{not json").unwrap();
        g.set_snapshot(Some(&bad));
        assert!(try_snapshot(Detail::Default).is_none());
        let _ = fs::remove_file(bad);
    }

    #[test]
    fn rust_caps_cards_at_eight_and_drops_javascript_href() {
        let mut cards = String::new();
        for i in 0..9 {
            cards.push_str(&format!(
                r#"{{"title":"car {i}","price":"$1.0{i}","href":"https://cars.com/{i}","rectCss":{{"left":1,"top":1,"width":10,"height":10}}}},"#
            ));
        }
        cards.push_str(
            r#"{"title":"js","price":"$0.00","href":"javascript:alert(1)","rectCss":{"left":1,"top":1,"width":10,"height":10}}"#,
        );
        let json = format!(
            r#"{{"url":"https://cars.com/search","title":"T","main_text":"x","metrics":{{"screenX":0,"screenY":0,"outerWidth":100,"outerHeight":100,"innerWidth":100,"innerHeight":100,"devicePixelRatio":1}},"elements":[],"cards":[{cards}]}}"#
        );
        let map = parse_snapshot_bytes(json.as_bytes()).expect("parse");
        assert_eq!(map.cards.len(), 8);
        assert!(
            map.cards
                .iter()
                .all(|c| !c.href.to_ascii_lowercase().starts_with("javascript:"))
        );
        assert_eq!(map.cards[0].title, "car 0");
        assert_eq!(map.cards[7].title, "car 7");
    }

    #[test]
    fn fixture_resolve_chr0_center() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(&EnvGuard::fixture_path()));
        let hit = try_resolve("chr:0").unwrap();
        assert_eq!(hit.rect.center(), (210, 166));
        assert_eq!(hit.role, "Edit");
    }

    #[test]
    fn missing_fixture_resolve_is_tool_error() {
        let g = EnvGuard::lock();
        g.set_snapshot(Some(std::path::Path::new(
            r"C:\dev\Helping-Hands\hands\tests\fixtures\missing-chrome.json",
        )));
        let err = try_resolve("chr:0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chr:"), "{msg}");
        assert!(msg.contains("fixture") || msg.contains("host"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn malformed_fixture_resolve_is_tool_error() {
        let g = EnvGuard::lock();
        let bad = std::env::temp_dir().join("hands-chrome-malformed.json");
        fs::write(&bad, "{\"url\":1}").unwrap();
        g.set_snapshot(Some(&bad));
        let err = try_resolve("chr:0").unwrap_err();
        let msg = err.to_string();
        let _ = fs::remove_file(bad);
        assert!(msg.contains("chr:") || msg.contains("fixture"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn silent_host_pipe_times_out_within_budget() {
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        let name = format!(
            r"\\.\pipe\hands-chrome-silent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let server_name = name.clone();
        let server = std::thread::spawn(move || {
            let wide: Vec<u16> = server_name
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    65_536,
                    65_536,
                    0,
                    None,
                )
            };
            if handle.is_invalid() {
                return;
            }
            let _ = ready_tx.send(());
            let _ = unsafe { ConnectNamedPipe(handle, None) };
            let _ = done_rx.recv();
            let _ = unsafe { DisconnectNamedPipe(handle) };
            let _ = unsafe { CloseHandle(handle) };
        });
        ready_rx.recv().expect("server created pipe");

        let g = EnvGuard::lock();
        g.set_snapshot(None);
        g.set_pipe(Some(&name));
        let started = Instant::now();
        let err = try_resolve("chr:0").unwrap_err();
        let elapsed = started.elapsed();
        drop(g);
        let _ = done_tx.send(());
        let _ = server.join();

        let msg = err.to_string();
        assert!(
            elapsed < Duration::from_millis(1_500),
            "client hung for {elapsed:?}: {msg}"
        );
        assert!(
            elapsed >= Duration::from_millis(50),
            "timeout too fast ({elapsed:?}): {msg}"
        );
        assert!(
            msg.contains("400") || msg.to_ascii_lowercase().contains("timed out"),
            "{msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn no_pipe_resolve_is_tool_error() {
        let g = EnvGuard::lock();
        g.set_snapshot(None);
        g.set_pipe(Some(r"\\.\pipe\hands-chrome-absent-0005-test"));
        let err = try_resolve("chr:0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chr:") || msg.contains("host"), "{msg}");
        assert!(
            !msg.to_ascii_lowercase().contains("unknown prefix"),
            "{msg}"
        );
    }

    #[test]
    fn git_ls_files_has_no_planning_paths() {
        let out = std::process::Command::new("git")
            .args(["ls-files"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("git ls-files");
        let text = String::from_utf8_lossy(&out.stdout);
        for needle in [
            "conductor/",
            "SHARED-UNDERSTANDING",
            "planner.md",
            "docs/adr",
            "CONTEXT.md",
        ] {
            assert!(
                !text.lines().any(|l| l.contains(needle)),
                "planning path leaked: {needle}"
            );
        }
    }

    #[test]
    fn extension_js_forbids_scrape_and_page_mutation() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension");
        let needles = [
            "setInterval",
            "MutationObserver",
            "eval(",
            "eval (",
            ".click(",
            "dispatchEvent",
            "innerHTML=",
            "innerHTML =",
            "chrome.scripting",
            "chrome.alarms",
        ];
        for name in ["content.js", "sw.js"] {
            let text = fs::read_to_string(root.join(name)).unwrap();
            for needle in needles {
                assert!(!text.contains(needle), "{name} must not contain {needle}");
            }
        }
    }
}
