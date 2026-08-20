//! Read-only native-host JSON / HKCU / pipe doctor.
//!
//! Diagnoses the 0011 paste/register path and prints one owner next step.
//! Does not write the registry, sideload, spawn `native-host`, or kill Chrome.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_EXPAND_SZ, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::PCWSTR;

use crate::chrome;
use crate::error::HandsError;
use crate::extract::Detail;
use crate::native_host;

const HKCU_SUBKEY: &str = r"Software\Google\Chrome\NativeMessagingHosts\com.helpinghands.host";
const MANIFEST_FILE: &str = "com.helpinghands.host.json";
const REG_ADD_KEY: &str = r"HKCU\Software\Google\Chrome\NativeMessagingHosts\com.helpinghands.host";

#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub json_text: Option<String>,
    pub json_path: Option<String>,
    pub convention_path: Option<String>,
    pub hkcu_default: Option<String>,
    pub hklm_default: Option<String>,
    pub pipe_up: bool,
    pub snapshot_ok: bool,
    pub snapshot_env_set: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ok: bool,
    pub next: String,
    pub expected_id: String,
    pub host_name: String,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Default)]
struct Builder {
    checks: Vec<Check>,
    next: Option<String>,
}

impl Builder {
    fn push(
        &mut self,
        id: &str,
        ok: bool,
        detail: impl Into<String>,
        fail_next: impl Into<String>,
    ) {
        self.checks.push(Check {
            id: id.to_string(),
            ok,
            detail: detail.into(),
        });
        if !ok && self.next.is_none() {
            self.next = Some(fail_next.into());
        }
    }

    fn skip(&mut self, id: &str) {
        self.checks.push(Check {
            id: id.to_string(),
            ok: false,
            detail: "not-run".into(),
        });
    }
}

enum JsonStatus {
    Missing,
    ExtraBytes,
    Invalid,
    Object(Value),
}

pub fn diagnose(inputs: Inputs) -> Report {
    let mut b = Builder::default();
    let expected_origin = expected_origin();
    let convention_disp = convention_display(&inputs);
    let rewrite_to = rewrite_target(&inputs);
    let advice_path = advice_json_path(&inputs);

    match classify_json(inputs.json_text.as_deref()) {
        JsonStatus::Missing => {
            let where_ = inputs
                .json_path
                .as_deref()
                .or(inputs.convention_path.as_deref())
                .unwrap_or(convention_disp.as_str());
            b.push(
                "json",
                false,
                format!("JSON missing ({where_})"),
                format!(
                    "Native-host JSON is missing. Rewrite via `hands native-host-manifest` and save to {rewrite_to}. Do not overwrite the git template."
                ),
            );
            b.skip("json_bytes");
            b.skip("name_type");
            b.skip("path");
            b.skip("allowed_origins");
        }
        JsonStatus::ExtraBytes => {
            b.push("json", true, present_detail(&inputs), "");
            b.push(
                "json_bytes",
                false,
                "extra bytes after first JSON object (PowerShell leftover)",
                format!(
                    "Native-host JSON has extra bytes after the first object (PowerShell leftover). Rewrite via `hands native-host-manifest` and save to {rewrite_to}. Do not overwrite the git template."
                ),
            );
            b.skip("name_type");
            b.skip("path");
            b.skip("allowed_origins");
        }
        JsonStatus::Invalid => {
            b.push("json", true, present_detail(&inputs), "");
            b.push(
                "json_bytes",
                false,
                "invalid JSON",
                format!(
                    "Native-host JSON is invalid. Rewrite via `hands native-host-manifest` and save to {rewrite_to}. Do not overwrite the git template."
                ),
            );
            b.skip("name_type");
            b.skip("path");
            b.skip("allowed_origins");
        }
        JsonStatus::Object(obj) => {
            b.push("json", true, present_detail(&inputs), "");
            b.push("json_bytes", true, "single JSON object", "");
            check_name_type(&mut b, &obj, &rewrite_to);
            check_path(&mut b, &obj, &rewrite_to);
            check_origins(&mut b, &obj, &expected_origin);
        }
    }

    let hkcu = trim_opt(inputs.hkcu_default.as_deref());
    let hklm = trim_opt(inputs.hklm_default.as_deref());
    match hkcu {
        None => {
            let detail = match hklm {
                Some(hklm) => format!("HKCU key missing (HKLM default is present: {hklm})"),
                None => "HKCU key missing".into(),
            };
            let next = format!(
                "Native messaging host host name is not registered. {detail}. Still register HKCU — run: {}   then reload the extension (Chrome caches the host list). Doctor did not write the registry.",
                reg_add_cmd(&advice_path)
            );
            b.push("hkcu", false, detail, next);
            b.skip("hkcu_value");
            b.skip("hkcu_file");
        }
        Some(hkcu) => {
            b.push("hkcu", true, format!("HKCU default is {hkcu}"), "");
            if is_unexpanded(hkcu) {
                let expanded = expand_advice(hkcu);
                b.push(
                    "hkcu_value",
                    false,
                    format!("unexpanded {hkcu}"),
                    format!(
                        "HKCU default is the literal {hkcu} — run: {}   then reload the extension (Chrome caches the host list). Doctor did not write the registry.",
                        reg_add_cmd(&expanded)
                    ),
                );
                b.skip("hkcu_file");
            } else if !Path::new(hkcu).is_file() {
                b.push(
                    "hkcu_value",
                    false,
                    format!("HKCU default is not an existing file ({hkcu})"),
                    format!(
                        "HKCU default is not an existing file ({hkcu}) — run: {}   then reload the extension (Chrome caches the host list). Doctor did not write the registry.",
                        reg_add_cmd(&advice_path)
                    ),
                );
                b.skip("hkcu_file");
            } else {
                b.push("hkcu_value", true, "HKCU default is an existing file", "");
                let already = inputs
                    .json_path
                    .as_deref()
                    .is_some_and(|p| paths_eq(p, hkcu));
                let same_convention = inputs
                    .convention_path
                    .as_deref()
                    .is_some_and(|c| paths_eq(c, hkcu));
                if already || same_convention {
                    let detail = if same_convention {
                        "HKCU default is the LOCALAPPDATA convention file".into()
                    } else {
                        format!("HKCU ≠ convention; Chrome loads HKCU path ({hkcu})")
                    };
                    b.push("hkcu_file", true, detail, "");
                } else {
                    b.push(
                        "hkcu_file",
                        false,
                        format!("HKCU points at a different existing file ({hkcu})"),
                        format!(
                            "HKCU default points at {hkcu}, which is a different file than the LOCALAPPDATA convention ({convention_disp}). Chrome loads the HKCU path — diagnose/rewrite that JSON. Then reload the extension (Chrome caches the host list)."
                        ),
                    );
                }
            }
        }
    }

    if inputs.snapshot_env_set {
        b.push(
            "snapshot_env",
            false,
            "HANDS_CHROME_SNAPSHOT is set",
            "HANDS_CHROME_SNAPSHOT is set — this is a host-double fixture, not a live host. Unset it for a live check.",
        );
    } else {
        b.push("snapshot_env", true, "HANDS_CHROME_SNAPSHOT unset", "");
    }

    if inputs.pipe_up {
        b.push("pipe", true, "pipe is up", "");
    } else {
        b.push(
            "pipe",
            false,
            "pipe down; host not connected",
            "Native host is not connected (pipe down). Reload the extension (Chrome caches the host list) or restart Chrome, then open an https tab (not chrome://).",
        );
    }

    if inputs.snapshot_env_set || !inputs.pipe_up {
        b.skip("snapshot");
    } else if inputs.snapshot_ok {
        b.push("snapshot", true, "snapshot ok within 400 ms", "");
    } else {
        b.push(
            "snapshot",
            false,
            "pipe up but snapshot failed within 400 ms",
            "Pipe is up but snapshot failed within 400 ms. Reload the extension (Chrome caches the host list).",
        );
    }

    let ok = b.next.is_none();
    let next = b.next.unwrap_or_else(|| {
        "Native host looks ready. If observe still has no chr: ids, open an https tab (not chrome://) and confirm the foreground window is Chrome.".into()
    });
    Report {
        ok,
        next,
        expected_id: native_host::EXTENSION_ID.to_string(),
        host_name: native_host::HOST_NAME.to_string(),
        checks: b.checks,
    }
}

pub fn run() -> Report {
    let convention_path = convention_json_path();
    let hkcu_default = read_host_default(Hive::Hkcu);
    let hklm_default = read_host_default(Hive::Hklm);
    let (json_text, json_path) = choose_json(convention_path.as_deref(), hkcu_default.as_deref());
    let pipe_up = pipe_present();
    let snapshot_env_set = snapshot_env_is_set();
    let snapshot_ok = if snapshot_env_set {
        false
    } else {
        chrome::try_snapshot(Detail::Default).is_some()
    };
    diagnose(Inputs {
        json_text,
        json_path,
        convention_path,
        hkcu_default,
        hklm_default,
        pipe_up,
        snapshot_ok,
        snapshot_env_set,
    })
}

pub fn serialize_report(report: &Report) -> Result<String, HandsError> {
    serde_json::to_string_pretty(report)
        .map_err(|err| HandsError::Chrome(format!("native-host-doctor: {err}")))
}

fn classify_json(text: Option<&str>) -> JsonStatus {
    let Some(text) = text else {
        return JsonStatus::Missing;
    };
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    match stream.next() {
        Some(Ok(value)) => {
            let rest = text.get(stream.byte_offset()..).unwrap_or("").trim();
            if !rest.is_empty() {
                JsonStatus::ExtraBytes
            } else if value.is_object() {
                JsonStatus::Object(value)
            } else {
                JsonStatus::Invalid
            }
        }
        _ => JsonStatus::Invalid,
    }
}

fn check_name_type(b: &mut Builder, obj: &Value, convention_disp: &str) {
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let name_ok = name == native_host::HOST_NAME;
    let type_ok = ty == "stdio";
    if name_ok && type_ok {
        b.push("name_type", true, format!("name={name} type={ty}"), "");
    } else {
        b.push(
            "name_type",
            false,
            format!("name={name:?} type={ty:?}"),
            format!(
                "`name` must be `{}` and `type` must be `stdio`. Rewrite via `hands native-host-manifest` and save to {convention_disp}. Do not overwrite the git template.",
                native_host::HOST_NAME
            ),
        );
    }
}

fn check_path(b: &mut Builder, obj: &Value, convention_disp: &str) {
    let path = obj.get("path").and_then(Value::as_str).unwrap_or("").trim();
    if path.eq_ignore_ascii_case("hands.exe") {
        b.push(
            "path",
            false,
            r#"path is the template "hands.exe""#,
            format!(
                r#"JSON `path` is the template `hands.exe`. Run `hands native-host-manifest` and save to {convention_disp}. Do not overwrite the git template."#
            ),
        );
        return;
    }
    if is_unexpanded(path) {
        b.push(
            "path",
            false,
            format!("JSON path is unexpanded ({path})"),
            format!(
                "JSON `path` is unexpanded (`$env:` or `%VAR%`). Rewrite the JSON via `hands native-host-manifest` with an absolute existing exe path and save to {convention_disp}. Do not overwrite the git template."
            ),
        );
        return;
    }
    if path.is_empty() || !Path::new(path).is_file() {
        b.push(
            "path",
            false,
            format!("path does not exist ({path})"),
            format!(
                "JSON `path` does not exist ({path}). Point `path` at an existing hands.exe via `hands native-host-manifest` and save to {convention_disp}. Do not overwrite the git template."
            ),
        );
        return;
    }
    b.push("path", true, format!("path exists ({path})"), "");
}

fn check_origins(b: &mut Builder, obj: &Value, expected: &str) {
    let origins: Vec<String> = obj
        .get("allowed_origins")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let has_expected = origins.iter().any(|o| o == expected);
    let has_wildcard = origins.iter().any(|o| o.contains('*'));
    if has_expected && !has_wildcard {
        b.push(
            "allowed_origins",
            true,
            format!("allowed_origins includes {expected}"),
            "",
        );
        return;
    }
    let detail = if has_wildcard {
        "allowed_origins contains wildcard *".into()
    } else {
        format!("allowed_origins missing {expected}")
    };
    b.push(
        "allowed_origins",
        false,
        detail,
        format!(
            "Access to the specified native messaging host is forbidden. `allowed_origins` must include `{expected}` (trailing slash required; Chrome does not allow wildcards such as `*`)."
        ),
    );
}

fn expected_origin() -> String {
    native_host::manifest_json(native_host::EXTENSION_ID, "hands.exe")
        .get("allowed_origins")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("chrome-extension://{}/", native_host::EXTENSION_ID))
}

fn convention_json_path() -> Option<String> {
    let la = std::env::var("LOCALAPPDATA").ok()?;
    let p = PathBuf::from(la).join("hands").join(MANIFEST_FILE);
    Some(p.to_string_lossy().into_owned())
}

fn convention_display(inputs: &Inputs) -> String {
    inputs
        .convention_path
        .clone()
        .or_else(convention_json_path)
        .unwrap_or_else(|| format!(r"%LOCALAPPDATA%\hands\{MANIFEST_FILE}"))
}

/// File Chrome will load (HKCU-named when that differs from convention).
fn rewrite_target(inputs: &Inputs) -> String {
    if let Some(p) = trim_opt(inputs.json_path.as_deref())
        && !is_unexpanded(p)
    {
        return p.to_string();
    }
    convention_display(inputs)
}

fn advice_json_path(inputs: &Inputs) -> String {
    if let Some(c) = inputs.convention_path.as_deref()
        && !is_unexpanded(c)
        && !c.is_empty()
    {
        return c.to_string();
    }
    expand_advice(&convention_display(inputs))
}

fn present_detail(inputs: &Inputs) -> String {
    match inputs.json_path.as_deref() {
        Some(p) => format!("JSON present ({p})"),
        None => "JSON present".into(),
    }
}

fn trim_opt(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

fn is_unexpanded(s: &str) -> bool {
    s.to_ascii_lowercase().contains("$env:") || contains_percent_var(s)
}

fn contains_percent_var(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let rest = i + 1;
            if let Some(off) = bytes[rest..].iter().position(|&b| b == b'%') {
                let name = &s[rest..rest + off];
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return true;
                }
                i = rest + off;
                continue;
            }
        }
        i += 1;
    }
    false
}

fn expand_advice(s: &str) -> String {
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        return replace_ci(
            replace_ci(s, "$env:LOCALAPPDATA", &la).as_str(),
            "%LOCALAPPDATA%",
            &la,
        );
    }
    s.to_string()
}

fn replace_ci(hay: &str, needle: &str, repl: &str) -> String {
    let h = hay.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(pos) = h[i..].find(&n) {
        let abs = i + pos;
        out.push_str(&hay[i..abs]);
        out.push_str(repl);
        i = abs + needle.len();
    }
    out.push_str(&hay[i..]);
    out
}

fn reg_add_cmd(json_path: &str) -> String {
    format!(r#"REG ADD "{REG_ADD_KEY}" /ve /t REG_SZ /d "{json_path}" /f"#)
}

fn paths_eq(a: &str, b: &str) -> bool {
    let pa = Path::new(a);
    let pb = Path::new(b);
    if let (Ok(ca), Ok(cb)) = (pa.canonicalize(), pb.canonicalize()) {
        return ca == cb;
    }
    let na = a
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    let nb = b
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    na == nb
}

fn choose_json(
    convention_path: Option<&str>,
    hkcu_default: Option<&str>,
) -> (Option<String>, Option<String>) {
    if let Some(hkcu) = trim_opt(hkcu_default)
        && !is_unexpanded(hkcu)
        && Path::new(hkcu).is_file()
    {
        let same_convention = convention_path.is_some_and(|c| paths_eq(c, hkcu));
        if !same_convention {
            return (std::fs::read_to_string(hkcu).ok(), Some(hkcu.to_string()));
        }
    }
    match convention_path {
        Some(p) if Path::new(p).is_file() => (std::fs::read_to_string(p).ok(), Some(p.to_string())),
        Some(p) => (None, Some(p.to_string())),
        None => (None, None),
    }
}

#[derive(Clone, Copy)]
enum Hive {
    Hkcu,
    Hklm,
}

fn read_host_default(hive: Hive) -> Option<String> {
    let root = match hive {
        Hive::Hkcu => HKEY_CURRENT_USER,
        Hive::Hklm => HKEY_LOCAL_MACHINE,
    };
    let sub = to_wide(HKCU_SUBKEY);
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(sub.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &raw mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut kind = REG_VALUE_TYPE::default();
    let mut nbytes = 0u32;
    let _ = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR::null(),
            None,
            Some(&raw mut kind),
            None,
            Some(&raw mut nbytes),
        )
    };
    if nbytes == 0 {
        let _ = unsafe { RegCloseKey(key) };
        return None;
    }
    let mut buf = vec![0u8; nbytes as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR::null(),
            None,
            Some(&raw mut kind),
            Some(buf.as_mut_ptr()),
            Some(&raw mut nbytes),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        return None;
    }
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        return None;
    }
    let u16s: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let raw = String::from_utf16_lossy(&u16s)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    if raw.is_empty() { None } else { Some(raw) }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pipe_present() -> bool {
    let name = native_host::pipe_name();
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let ready = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 0) };
    ready.as_bool()
}

fn snapshot_env_is_set() -> bool {
    std::env::var(chrome::SNAPSHOT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_workspace() -> (TempDir, String, String) {
        let dir = std::env::temp_dir().join(format!("hands-doctor-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("hands-real.exe");
        fs::write(&exe, b"dummy").unwrap();
        let json_path = dir.join(MANIFEST_FILE);
        let exe_s = exe.to_string_lossy().into_owned();
        let value = native_host::manifest_json(native_host::EXTENSION_ID, &exe_s);
        fs::write(&json_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        let json_s = json_path.to_string_lossy().into_owned();
        (TempDir(dir), exe_s, json_s)
    }

    fn good_inputs(json_path: &str) -> Inputs {
        let json_text = fs::read_to_string(json_path).unwrap();
        Inputs {
            json_text: Some(json_text),
            json_path: Some(json_path.to_string()),
            convention_path: Some(json_path.to_string()),
            hkcu_default: Some(json_path.to_string()),
            hklm_default: None,
            pipe_up: true,
            snapshot_ok: true,
            snapshot_env_set: false,
        }
    }

    fn write_json(path: &str, value: &Value) {
        fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    #[test]
    fn extra_bytes_after_object_owns_next() {
        let (_dir, exe, json_path) = temp_workspace();
        let clean =
            serde_json::to_string(&native_host::manifest_json(native_host::EXTENSION_ID, &exe))
                .unwrap();
        let dirty = format!(
            "{clean}\nREG ADD \"HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.helpinghands.host\" /ve"
        );
        let report = diagnose(Inputs {
            json_text: Some(dirty),
            json_path: Some(json_path),
            ..Inputs::default()
        });
        assert!(!report.ok);
        assert!(
            report.next.contains("extra bytes"),
            "expected extra-bytes next, got {}",
            report.next
        );
        assert!(report.next.contains("native-host-manifest"));
        assert!(!report.next.contains("invalid JSON"));
    }

    #[test]
    fn unexpanded_env_dollar_hkcu_prints_reg_add() {
        let (_dir, _exe, json_path) = temp_workspace();
        let la = std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA");
        let raw = r"$env:LOCALAPPDATA\hands\com.helpinghands.host.json";
        let mut inputs = good_inputs(&json_path);
        inputs.hkcu_default = Some(raw.into());
        let report = diagnose(inputs);
        assert!(!report.ok);
        let expanded = format!(r"{la}\hands\com.helpinghands.host.json");
        assert!(report.next.contains("REG ADD"), "next={}", report.next);
        assert!(
            report.next.contains(&expanded),
            "next missing expanded path {expanded}: {}",
            report.next
        );
        assert!(report.next.to_ascii_lowercase().contains("did not write"));
    }

    #[test]
    fn unexpanded_percent_localappdata_hkcu_prints_reg_add() {
        let (_dir, _exe, json_path) = temp_workspace();
        let la = std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA");
        let raw = r"%LOCALAPPDATA%\hands\com.helpinghands.host.json";
        let mut inputs = good_inputs(&json_path);
        inputs.hkcu_default = Some(raw.into());
        let report = diagnose(inputs);
        assert!(!report.ok);
        let expanded = format!(r"{la}\hands\com.helpinghands.host.json");
        assert!(report.next.contains("REG ADD"));
        assert!(
            report.next.contains(&expanded),
            "next missing expanded path {expanded}: {}",
            report.next
        );
        assert!(report.next.to_ascii_lowercase().contains("did not write"));
    }

    #[test]
    fn invalid_json_is_distinct_from_extra_bytes() {
        let report = diagnose(Inputs {
            json_text: Some("not-json {".into()),
            json_path: Some(r"C:\missing\com.helpinghands.host.json".into()),
            convention_path: Some(r"C:\missing\com.helpinghands.host.json".into()),
            ..Inputs::default()
        });
        assert!(!report.ok);
        assert!(
            report.next.contains("invalid"),
            "expected invalid-JSON next, got {}",
            report.next
        );
        assert!(!report.next.contains("extra bytes"));
        assert!(report.next.contains("native-host-manifest"));
    }

    #[test]
    fn choose_json_reads_hkcu_file_when_different_from_convention() {
        let (_dir, exe, convention) = temp_workspace();
        let other_dir =
            std::env::temp_dir().join(format!("hands-doctor-hkcu-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&other_dir).unwrap();
        let other_json = other_dir.join(MANIFEST_FILE);
        let mut value = native_host::manifest_json(native_host::EXTENSION_ID, &exe);
        value["description"] = serde_json::json!("hkcu-file");
        fs::write(&other_json, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        let other_s = other_json.to_string_lossy().into_owned();
        let (text, path) = choose_json(Some(&convention), Some(&other_s));
        let _ = fs::remove_dir_all(&other_dir);
        assert_eq!(path.as_deref(), Some(other_s.as_str()));
        assert!(
            text.is_some_and(|t| t.contains("hkcu-file")),
            "live reader must diagnose the HKCU-named file, not the convention file"
        );
    }

    #[test]
    fn hkcu_other_file_extra_bytes_next_names_that_file() {
        let (_dir, exe, convention) = temp_workspace();
        let other_dir =
            std::env::temp_dir().join(format!("hands-doctor-hkcu-dirty-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&other_dir).unwrap();
        let other_json = other_dir.join(MANIFEST_FILE);
        let clean =
            serde_json::to_string(&native_host::manifest_json(native_host::EXTENSION_ID, &exe))
                .unwrap();
        let dirty = format!("{clean}\nWrite-Host leftover");
        fs::write(&other_json, &dirty).unwrap();
        let other_s = other_json.to_string_lossy().into_owned();
        let report = diagnose(Inputs {
            json_text: Some(dirty),
            json_path: Some(other_s.clone()),
            convention_path: Some(convention.clone()),
            hkcu_default: Some(other_s.clone()),
            pipe_up: true,
            snapshot_ok: true,
            snapshot_env_set: false,
            ..Inputs::default()
        });
        let _ = fs::remove_dir_all(&other_dir);
        assert!(!report.ok);
        assert!(
            report.next.contains("extra bytes") && report.next.contains(&other_s),
            "next must name the HKCU-loaded file, got {}",
            report.next
        );
        assert!(
            !report.next.contains(&convention),
            "next must not send the owner to the unused convention file: {}",
            report.next
        );
    }

    #[test]
    fn missing_json_next() {
        let report = diagnose(Inputs {
            json_text: None,
            json_path: Some(r"C:\missing\com.helpinghands.host.json".into()),
            convention_path: Some(r"C:\missing\com.helpinghands.host.json".into()),
            ..Inputs::default()
        });
        assert!(!report.ok);
        assert!(report.next.contains("native-host-manifest"));
        assert!(
            report.next.to_ascii_lowercase().contains("missing")
                || report.next.contains("JSON is missing")
        );
        assert!(report.next.contains("git template"));
    }

    #[test]
    fn missing_hkcu_mentions_hklm() {
        let (_dir, _exe, json_path) = temp_workspace();
        let mut inputs = good_inputs(&json_path);
        inputs.hkcu_default = None;
        inputs.hklm_default = Some(r"C:\ProgramData\hands\com.helpinghands.host.json".into());
        let report = diagnose(inputs);
        assert!(!report.ok);
        assert!(
            report.next.contains("HKLM") || report.next.to_ascii_lowercase().contains("hklm"),
            "next={}",
            report.next
        );
        assert!(
            report.next.contains("not registered") || report.next.contains("HKCU"),
            "next={}",
            report.next
        );
        assert!(report.next.to_ascii_lowercase().contains("did not write"));
    }

    #[test]
    fn template_path_hands_exe() {
        let (_dir, _exe, json_path) = temp_workspace();
        let value = native_host::manifest_json(native_host::EXTENSION_ID, "hands.exe");
        write_json(&json_path, &value);
        let mut inputs = good_inputs(&json_path);
        inputs.json_text = Some(serde_json::to_string(&value).unwrap());
        let report = diagnose(inputs);
        assert!(!report.ok);
        assert!(
            report.next.contains("hands.exe") && report.next.contains("native-host-manifest"),
            "next={}",
            report.next
        );
        assert!(report.next.contains("git template"));
    }

    #[test]
    fn missing_exe_path() {
        let (_dir, _exe, json_path) = temp_workspace();
        let missing = r"C:\hands-doctor-missing\nope.exe";
        let value = native_host::manifest_json(native_host::EXTENSION_ID, missing);
        write_json(&json_path, &value);
        let mut inputs = good_inputs(&json_path);
        inputs.json_text = Some(serde_json::to_string(&value).unwrap());
        let report = diagnose(inputs);
        assert!(!report.ok);
        assert!(
            report.next.contains("does not exist") && report.next.contains(missing),
            "next={}",
            report.next
        );
    }

    #[test]
    fn wrong_origin_missing_committed_id() {
        let (_dir, exe, json_path) = temp_workspace();
        let mut value = native_host::manifest_json(native_host::EXTENSION_ID, &exe);
        value["allowed_origins"] =
            serde_json::json!(["chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"]);
        write_json(&json_path, &value);
        let mut inputs = good_inputs(&json_path);
        inputs.json_text = Some(serde_json::to_string(&value).unwrap());
        let report = diagnose(inputs);
        assert!(!report.ok);
        assert!(
            report.next.contains(native_host::EXTENSION_ID) && report.next.contains("forbidden"),
            "next={}",
            report.next
        );
        assert!(report.next.contains(&format!(
            "chrome-extension://{}/",
            native_host::EXTENSION_ID
        )));
    }

    #[test]
    fn star_origin_forbidden() {
        let (_dir, exe, json_path) = temp_workspace();
        let mut value = native_host::manifest_json(native_host::EXTENSION_ID, &exe);
        value["allowed_origins"] = serde_json::json!(["*"]);
        write_json(&json_path, &value);
        let mut inputs = good_inputs(&json_path);
        inputs.json_text = Some(serde_json::to_string(&value).unwrap());
        let report = diagnose(inputs);
        assert!(!report.ok);
        assert!(
            report.next.contains('*')
                || report.next.contains("wildcard")
                || report.next.contains("forbidden"),
            "next={}",
            report.next
        );
    }

    #[test]
    fn snapshot_env_not_live_host() {
        let (_dir, _exe, json_path) = temp_workspace();
        let mut inputs = good_inputs(&json_path);
        inputs.snapshot_env_set = true;
        inputs.snapshot_ok = true;
        let report = diagnose(inputs);
        assert!(!report.ok);
        let lower = report.next.to_ascii_lowercase();
        assert!(
            lower.contains("snapshot") || lower.contains("fixture") || lower.contains("not a live"),
            "next={}",
            report.next
        );
        assert!(!lower.contains("looks ready"));
    }

    #[test]
    fn all_good_injected_inputs() {
        let (_dir, _exe, json_path) = temp_workspace();
        let report = diagnose(good_inputs(&json_path));
        assert!(report.ok, "next={}", report.next);
        assert!(
            report.next.to_ascii_lowercase().contains("ready") && report.next.contains("https"),
            "next={}",
            report.next
        );
        assert_eq!(report.expected_id, native_host::EXTENSION_ID);
        assert_eq!(report.host_name, native_host::HOST_NAME);
        assert!(report.checks.iter().all(|c| c.ok));
    }

    #[test]
    fn host_doctor_source_forbids_write_and_kill() {
        let src = include_str!("host_doctor.rs");
        let set = concat!("RegSet", "ValueExW");
        let create = concat!("RegCreate", "KeyExW");
        let del_key = concat!("RegDelete", "KeyW");
        let del_val = concat!("RegDelete", "ValueW");
        let kill = concat!("Terminate", "Process");
        let task = concat!("task", "kill");
        let reg_exe = concat!("reg", ".exe");
        assert!(!src.contains(set), "host_doctor.rs must not {set}");
        assert!(!src.contains(create), "host_doctor.rs must not {create}");
        assert!(!src.contains(del_key), "host_doctor.rs must not {del_key}");
        assert!(!src.contains(del_val), "host_doctor.rs must not {del_val}");
        assert!(!src.contains(kill), "host_doctor.rs must not {kill}");
        assert!(
            !src.to_ascii_lowercase().contains(task),
            "host_doctor.rs must not {task}"
        );
        assert!(!src.contains(reg_exe), "host_doctor.rs must not {reg_exe}");
        assert!(src.contains("REG ADD"), "advice string REG ADD is allowed");
        let pr9 = concat!("PR #", "9");
        let leftover = concat!("PeekNamedPipe ", "leftover");
        let stop = concat!("stop and ", "report");
        let patch = concat!("do not ", "patch");
        assert!(!src.contains(pr9), "host_doctor.rs must not name {pr9}");
        assert!(
            !src.contains(leftover),
            "host_doctor.rs must not name {leftover}"
        );
        assert!(!src.contains(stop), "host_doctor.rs must not say {stop}");
        assert!(!src.contains(patch), "host_doctor.rs must not say {patch}");
    }

    #[test]
    fn snapshot_fail_reload_not_leftover() {
        let (_dir, _exe, json_path) = temp_workspace();
        let mut inputs = good_inputs(&json_path);
        inputs.pipe_up = true;
        inputs.snapshot_ok = false;
        inputs.snapshot_env_set = false;
        let report = diagnose(inputs);
        assert!(!report.ok);
        let lower = report.next.to_ascii_lowercase();
        assert!(
            lower.contains("reload") && lower.contains("extension"),
            "next={}",
            report.next
        );
        let pr9 = concat!("pr #", "9");
        let leftover = concat!("peeknamedpipe ", "leftover");
        let stop = concat!("stop and ", "report");
        let patch = concat!("do not ", "patch");
        assert!(!lower.contains(pr9), "next={}", report.next);
        assert!(!lower.contains(leftover), "next={}", report.next);
        assert!(!lower.contains(stop), "next={}", report.next);
        assert!(!lower.contains(patch), "next={}", report.next);
    }
}
