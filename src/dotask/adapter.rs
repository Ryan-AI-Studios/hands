//! Provider resolve, allowlist, auth headers, and per-wire encode/parse.

use serde_json::{Value, json};

use crate::error::HandsError;

use super::{
    DEFAULT_BASE, DEFAULT_MODEL, KEY_ENV_FALLBACK, KEY_ENV_PRIMARY, KEY_ENV_SIMPLE,
    MAX_OUTPUT_TOKENS, SYSTEM_PROMPT,
};

pub const PROVIDER_ENV: &str = "HANDS_DOTASK_PROVIDER";
pub const DOTASK_MODEL_ENV: &str = "HANDS_DOTASK_MODEL";
pub const DOTASK_BASE_ENV: &str = "HANDS_DOTASK_BASE_URL";

const FORBIDDEN_GEMMA_PORT: u16 = 80 * 100 + 81;
const FORBIDDEN_EMBED_PORT: u16 = 80 * 100 + 83;

const DEFAULT_MODEL_OPENAI: &str = "gpt-5.6-terra";
const DEFAULT_MODEL_ANTHROPIC: &str = "claude-sonnet-5";
const DEFAULT_MODEL_GOOGLE: &str = "gemini-3.7-flash";
const DEFAULT_MODEL_OLLAMA: &str = "gpt-oss:120b";
const DEFAULT_MODEL_DEEPSEEK: &str = "deepseek-v4-flash";
const DEFAULT_MODEL_ZHIPU: &str = "glm-5.2";

const DEFAULT_BASE_OPENAI: &str = "https://api.openai.com/v1";
const DEFAULT_BASE_ANTHROPIC: &str = "https://api.anthropic.com/v1";
const DEFAULT_BASE_GOOGLE: &str = "https://generativelanguage.googleapis.com/v1beta";
const DEFAULT_BASE_OLLAMA: &str = "https://ollama.com/api";
const DEFAULT_BASE_DEEPSEEK: &str = "https://api.deepseek.com";
const DEFAULT_BASE_ZHIPU: &str = "https://api.z.ai/api/paas/v4";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Xai,
    Openai,
    Anthropic,
    Google,
    Ollama,
    Deepseek,
    Zhipu,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::Ollama => "ollama",
            Self::Deepseek => "deepseek",
            Self::Zhipu => "zhipu",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Xai => DEFAULT_MODEL,
            Self::Openai => DEFAULT_MODEL_OPENAI,
            Self::Anthropic => DEFAULT_MODEL_ANTHROPIC,
            Self::Google => DEFAULT_MODEL_GOOGLE,
            Self::Ollama => DEFAULT_MODEL_OLLAMA,
            Self::Deepseek => DEFAULT_MODEL_DEEPSEEK,
            Self::Zhipu => DEFAULT_MODEL_ZHIPU,
        }
    }

    pub fn default_base(self) -> &'static str {
        match self {
            Self::Xai => DEFAULT_BASE,
            Self::Openai => DEFAULT_BASE_OPENAI,
            Self::Anthropic => DEFAULT_BASE_ANTHROPIC,
            Self::Google => DEFAULT_BASE_GOOGLE,
            Self::Ollama => DEFAULT_BASE_OLLAMA,
            Self::Deepseek => DEFAULT_BASE_DEEPSEEK,
            Self::Zhipu => DEFAULT_BASE_ZHIPU,
        }
    }

    fn official_hosts(self) -> &'static [&'static str] {
        match self {
            Self::Xai => &["api.x.ai"],
            Self::Openai => &["api.openai.com"],
            Self::Anthropic => &["api.anthropic.com"],
            Self::Google => &["generativelanguage.googleapis.com"],
            Self::Ollama => &["ollama.com"],
            Self::Deepseek => &["api.deepseek.com"],
            Self::Zhipu => &["api.z.ai", "open.bigmodel.cn"],
        }
    }

    fn default_prefix(self) -> &'static str {
        match self {
            Self::Xai | Self::Openai | Self::Anthropic => "/v1",
            Self::Google => "/v1beta",
            Self::Ollama => "/api",
            Self::Deepseek => "",
            Self::Zhipu => "/api/paas/v4",
        }
    }

    pub fn key_envs(self) -> &'static [&'static str] {
        match self {
            Self::Xai => &[KEY_ENV_PRIMARY, KEY_ENV_FALLBACK],
            Self::Openai => &["OPENAI_API_KEY"],
            Self::Anthropic => &["ANTHROPIC_API_KEY"],
            Self::Google => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            Self::Ollama => &["OLLAMA_API_KEY"],
            Self::Deepseek => &["DEEPSEEK_API_KEY"],
            Self::Zhipu => &["ZHIPU_API_KEY", "ZAI_API_KEY"],
        }
    }

    fn wire(self) -> Wire {
        match self {
            Self::Xai | Self::Openai => Wire::Responses,
            Self::Anthropic => Wire::Messages,
            Self::Google => Wire::Interactions,
            Self::Ollama => Wire::OllamaChat,
            Self::Deepseek | Self::Zhipu => Wire::ChatCompletions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    Responses,
    Messages,
    Interactions,
    ChatCompletions,
    OllamaChat,
}

#[derive(Debug, Clone)]
pub(super) struct FnCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub(super) struct ModelTurn {
    pub calls: Vec<FnCall>,
    pub text: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum TurnItem {
    Call(FnCall),
    Result {
        call_id: String,
        name: String,
        output: String,
    },
}

pub fn parse_provider(raw: &str) -> Result<Provider, HandsError> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(Provider::Xai);
    }
    match s.to_ascii_lowercase().as_str() {
        "xai" => Ok(Provider::Xai),
        "openai" | "chatgpt" => Ok(Provider::Openai),
        "anthropic" | "claude" => Ok(Provider::Anthropic),
        "google" | "gemini" => Ok(Provider::Google),
        "ollama" => Ok(Provider::Ollama),
        "deepseek" => Ok(Provider::Deepseek),
        "zhipu" | "glm" => Ok(Provider::Zhipu),
        other => Err(HandsError::DoTask(format!("unknown provider '{other}'"))),
    }
}

pub fn resolve_provider() -> Result<Provider, HandsError> {
    parse_provider(&std::env::var(PROVIDER_ENV).unwrap_or_default())
}

pub fn parse_xai_base(raw: &str) -> Result<String, HandsError> {
    parse_base(Provider::Xai, raw)
}

pub fn parse_base(provider: Provider, raw: &str) -> Result<String, HandsError> {
    let raw = raw.trim();
    let raw = if raw.is_empty() {
        provider.default_base()
    } else {
        raw
    };
    if raw.contains('@') {
        return Err(HandsError::DoTask(
            "base URL must not include userinfo".into(),
        ));
    }
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = raw.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err(HandsError::DoTask(
            "base URL must be http:// or https://".into(),
        ));
    };
    let rest = rest.trim_end_matches('/');
    let rest = strip_hop_suffix(rest);
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if hostport.is_empty() {
        return Err(HandsError::DoTask("base URL host is empty".into()));
    }
    let host = host_of(hostport);
    if host == "0.0.0.0" {
        return Err(HandsError::DoTask(
            "base URL host must not be 0.0.0.0".into(),
        ));
    }
    let port = port_of(hostport, scheme);
    if port == FORBIDDEN_GEMMA_PORT || port == FORBIDDEN_EMBED_PORT {
        return Err(HandsError::DoTask(
            "base URL must not use loopback ports 8081 or 8083".into(),
        ));
    }
    let official = provider
        .official_hosts()
        .iter()
        .any(|h| host.eq_ignore_ascii_case(h));
    let loopback = host.eq_ignore_ascii_case("127.0.0.1") || host.eq_ignore_ascii_case("localhost");
    if official {
        if scheme != "https" {
            return Err(HandsError::DoTask(format!(
                "base URL for {host} must be https"
            )));
        }
        let prefix = provider.default_prefix();
        if !path_allowed(path, prefix) {
            return Err(HandsError::DoTask(format!(
                "base URL path must be empty or {prefix} (got '{path}')"
            )));
        }
        let path = if path.is_empty() { prefix } else { path };
        return Ok(format!("https://{hostport}{path}"));
    }
    if loopback {
        if scheme != "http" {
            return Err(HandsError::DoTask("base URL loopback must be http".into()));
        }
        let prefix = provider.default_prefix();
        if !path_allowed(path, prefix) {
            return Err(HandsError::DoTask(format!(
                "base URL path must be empty or {prefix} (got '{path}')"
            )));
        }
        return Ok(format!("http://{hostport}{path}"));
    }
    Err(HandsError::DoTask(format!(
        "base URL host is not allowlisted (got '{host}')"
    )))
}

fn strip_hop_suffix(rest: &str) -> &str {
    rest.strip_suffix("/chat/completions")
        .or_else(|| rest.strip_suffix("/responses"))
        .or_else(|| rest.strip_suffix("/messages"))
        .or_else(|| rest.strip_suffix("/interactions"))
        .or_else(|| rest.strip_suffix("/chat"))
        .unwrap_or(rest)
        .trim_end_matches('/')
}

fn host_of(hostport: &str) -> &str {
    hostport.split(':').next().unwrap_or(hostport)
}

fn port_of(hostport: &str, scheme: &str) -> u16 {
    hostport
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(if scheme == "https" { 443 } else { 80 })
}

fn path_allowed(path: &str, prefix: &str) -> bool {
    path.is_empty() || (!prefix.is_empty() && path == prefix)
}

pub(super) fn resolve_base_raw(provider: Provider) -> String {
    if let Some(v) = nonempty_env(DOTASK_BASE_ENV) {
        return v;
    }
    if provider == Provider::Xai
        && let Some(v) = nonempty_env(super::BASE_ENV)
    {
        return v;
    }
    String::new()
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(super) fn hop_url(provider: Provider, base: &str) -> String {
    match provider {
        Provider::Xai | Provider::Openai => format!("{base}/responses"),
        Provider::Anthropic => format!("{base}/messages"),
        Provider::Google => format!("{base}/interactions"),
        Provider::Deepseek | Provider::Zhipu => format!("{base}/chat/completions"),
        Provider::Ollama => {
            if base.ends_with("/api") {
                format!("{base}/chat")
            } else if is_http_loopback(base) {
                format!("{base}/api/chat")
            } else {
                format!("{base}/chat")
            }
        }
    }
}

pub(super) fn is_http_loopback(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or("");
    host.eq_ignore_ascii_case("127.0.0.1") || host.eq_ignore_ascii_case("localhost")
}

pub fn auth_headers(provider: Provider, key: Option<&str>) -> Vec<(String, String)> {
    match (provider, key) {
        (Provider::Anthropic, Some(k)) => vec![
            ("x-api-key".into(), k.to_string()),
            ("anthropic-version".into(), "2023-06-01".into()),
        ],
        (Provider::Google, Some(k)) => vec![("x-goog-api-key".into(), k.to_string())],
        (_, Some(k)) => vec![("Authorization".into(), format!("Bearer {k}"))],
        (Provider::Ollama, None) => Vec::new(),
        (_, None) => Vec::new(),
    }
}

pub fn resolve_api_key() -> Option<String> {
    resolve_api_key_for(resolve_provider().unwrap_or(Provider::Xai))
}

pub fn resolve_api_key_for(provider: Provider) -> Option<String> {
    let mut names = Vec::with_capacity(1 + provider.key_envs().len());
    names.push(KEY_ENV_SIMPLE);
    names.extend_from_slice(provider.key_envs());
    for key in names {
        if let Some(v) = nonempty_env(key) {
            return Some(v);
        }
    }
    None
}

pub fn resolve_model(explicit: Option<&str>) -> String {
    resolve_model_for(resolve_provider().unwrap_or(Provider::Xai), explicit)
}

pub(super) fn resolve_model_for(provider: Provider, explicit: Option<&str>) -> String {
    if let Some(v) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return v.to_string();
    }
    if let Some(v) = nonempty_env(DOTASK_MODEL_ENV) {
        return v;
    }
    if let Some(v) = nonempty_env(super::MODEL_ENV) {
        return v;
    }
    provider.default_model().to_string()
}

pub(super) fn missing_key_message(provider: Provider) -> String {
    let extra = provider.key_envs();
    match extra {
        [] => format!("missing API key (set {KEY_ENV_SIMPLE})"),
        [b] => format!("missing API key (set {KEY_ENV_SIMPLE} or {b})"),
        [mid @ .., last] => format!(
            "missing API key (set {KEY_ENV_SIMPLE}, {}, or {last})",
            mid.join(", ")
        ),
    }
}

pub(super) fn key_required(provider: Provider, base: &str) -> bool {
    !(provider == Provider::Ollama && is_http_loopback(base))
}

pub(super) fn encode_request(
    provider: Provider,
    model: &str,
    goal: &str,
    items: &[TurnItem],
    image_b64: Option<&str>,
    tools: &Value,
) -> Value {
    match provider.wire() {
        Wire::Responses => encode_responses(model, goal, items, image_b64, tools),
        Wire::Messages => encode_messages(model, goal, items, image_b64, tools),
        Wire::Interactions => encode_interactions(model, goal, items, image_b64, tools),
        Wire::ChatCompletions => encode_chat_completions(model, goal, items, image_b64, tools),
        Wire::OllamaChat => encode_ollama(model, goal, items, image_b64, tools),
    }
}

fn encode_responses(
    model: &str,
    goal: &str,
    items: &[TurnItem],
    image_b64: Option<&str>,
    tools: &Value,
) -> Value {
    let mut input = vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": goal}),
    ];
    for item in items {
        match item {
            TurnItem::Call(call) => input.push(function_call_item(call)),
            TurnItem::Result {
                call_id, output, ..
            } => input.push(function_call_output(call_id, output)),
        }
    }
    if let Some(b64) = image_b64 {
        input.push(json!({
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": format!("data:image/png;base64,{b64}"),
                "detail": "low"
            }]
        }));
    }
    json!({
        "model": model,
        "input": input,
        "tools": tools,
        "stream": false,
        "parallel_tool_calls": false,
        "store": false,
        "max_output_tokens": MAX_OUTPUT_TOKENS,
    })
}

fn encode_messages(
    model: &str,
    goal: &str,
    items: &[TurnItem],
    image_b64: Option<&str>,
    tools: &Value,
) -> Value {
    let mut messages = Vec::new();
    let mut user_parts: Vec<Value> = vec![json!({"type": "text", "text": goal})];
    let mut assistant_parts: Vec<Value> = Vec::new();
    for item in items {
        match item {
            TurnItem::Call(call) => {
                if !user_parts.is_empty() {
                    messages.push(json!({"role": "user", "content": user_parts}));
                    user_parts = Vec::new();
                }
                assistant_parts.push(json!({
                    "type": "tool_use",
                    "id": call.call_id,
                    "name": call.name,
                    "input": parse_args(&call.arguments)
                }));
            }
            TurnItem::Result {
                call_id, output, ..
            } => {
                if !assistant_parts.is_empty() {
                    messages.push(json!({"role": "assistant", "content": assistant_parts}));
                    assistant_parts = Vec::new();
                }
                user_parts.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": output
                }));
            }
        }
    }
    if !assistant_parts.is_empty() {
        messages.push(json!({"role": "assistant", "content": assistant_parts}));
    }
    if let Some(b64) = image_b64 {
        user_parts.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": b64
            }
        }));
    }
    if !user_parts.is_empty() {
        messages.push(json!({"role": "user", "content": user_parts}));
    }
    json!({
        "model": model,
        "max_tokens": MAX_OUTPUT_TOKENS,
        "system": SYSTEM_PROMPT,
        "messages": messages,
        "tools": messages_tools(tools),
        "stream": false,
    })
}

fn encode_interactions(
    model: &str,
    goal: &str,
    items: &[TurnItem],
    image_b64: Option<&str>,
    tools: &Value,
) -> Value {
    let mut input = vec![
        json!({
            "type": "user_input",
            "content": [{"type": "text", "text": SYSTEM_PROMPT}]
        }),
        json!({
            "type": "user_input",
            "content": [{"type": "text", "text": goal}]
        }),
    ];
    for item in items {
        match item {
            TurnItem::Call(call) => {
                input.push(json!({
                    "type": "function_call",
                    "id": call.call_id,
                    "name": call.name,
                    "arguments": parse_args(&call.arguments)
                }));
            }
            TurnItem::Result {
                call_id,
                name,
                output,
            } => {
                input.push(json!({
                    "type": "function_result",
                    "name": name,
                    "call_id": call_id,
                    "result": [{"type": "text", "text": output}]
                }));
            }
        }
    }
    if let Some(b64) = image_b64 {
        input.push(json!({
            "type": "user_input",
            "content": [{
                "type": "image",
                "mime_type": "image/png",
                "data": b64
            }]
        }));
    }
    json!({
        "model": model,
        "input": input,
        "tools": tools,
        "stream": false,
        "store": false,
    })
}

fn encode_chat_completions(
    model: &str,
    goal: &str,
    items: &[TurnItem],
    image_b64: Option<&str>,
    tools: &Value,
) -> Value {
    let mut messages = vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": goal}),
    ];
    let mut pending_calls: Vec<Value> = Vec::new();
    for item in items {
        match item {
            TurnItem::Call(call) => {
                pending_calls.push(json!({
                    "id": call.call_id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": args_as_string(&call.arguments)
                    }
                }));
            }
            TurnItem::Result {
                call_id, output, ..
            } => {
                if !pending_calls.is_empty() {
                    messages.push(json!({
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": pending_calls
                    }));
                    pending_calls = Vec::new();
                }
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output
                }));
            }
        }
    }
    if !pending_calls.is_empty() {
        messages.push(json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": pending_calls
        }));
    }
    if let Some(b64) = image_b64 {
        messages.push(json!({
            "role": "user",
            "content": [{
                "type": "image_url",
                "image_url": {"url": format!("data:image/png;base64,{b64}")}
            }]
        }));
    }
    json!({
        "model": model,
        "messages": messages,
        "tools": nested_tools(tools),
        "stream": false,
        "parallel_tool_calls": false,
        "max_tokens": MAX_OUTPUT_TOKENS,
    })
}

fn encode_ollama(
    model: &str,
    goal: &str,
    items: &[TurnItem],
    image_b64: Option<&str>,
    tools: &Value,
) -> Value {
    let mut messages = vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": goal}),
    ];
    let mut pending_calls: Vec<Value> = Vec::new();
    for item in items {
        match item {
            TurnItem::Call(call) => {
                pending_calls.push(json!({
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": parse_args(&call.arguments)
                    }
                }));
            }
            TurnItem::Result { name, output, .. } => {
                if !pending_calls.is_empty() {
                    messages.push(json!({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": pending_calls
                    }));
                    pending_calls = Vec::new();
                }
                messages.push(json!({
                    "role": "tool",
                    "tool_name": name,
                    "content": output
                }));
            }
        }
    }
    if !pending_calls.is_empty() {
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": pending_calls
        }));
    }
    if let Some(b64) = image_b64 {
        messages.push(json!({
            "role": "user",
            "content": "",
            "images": [b64]
        }));
    }
    json!({
        "model": model,
        "messages": messages,
        "tools": nested_tools(tools),
        "stream": false,
    })
}

fn messages_tools(tools: &Value) -> Value {
    let arr = tools.as_array().cloned().unwrap_or_default();
    Value::Array(
        arr.into_iter()
            .map(|t| {
                json!({
                    "name": t.get("name").cloned().unwrap_or(json!("")),
                    "description": t.get("description").cloned().unwrap_or(json!("")),
                    "input_schema": t.get("parameters").cloned().unwrap_or(json!({"type":"object"}))
                })
            })
            .collect(),
    )
}

fn nested_tools(tools: &Value) -> Value {
    let arr = tools.as_array().cloned().unwrap_or_default();
    Value::Array(
        arr.into_iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").cloned().unwrap_or(json!("")),
                        "description": t.get("description").cloned().unwrap_or(json!("")),
                        "parameters": t.get("parameters").cloned().unwrap_or(json!({"type":"object"}))
                    }
                })
            })
            .collect(),
    )
}

fn function_call_item(call: &FnCall) -> Value {
    json!({
        "type": "function_call",
        "call_id": call.call_id,
        "name": call.name,
        "arguments": args_as_string(&call.arguments)
    })
}

fn function_call_output(call_id: &str, output: &str) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output
    })
}

fn args_as_string(args: &Value) -> String {
    match args {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub(super) fn parse_args(raw: &Value) -> Value {
    match raw {
        Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
        Value::Object(_) => raw.clone(),
        _ => json!({}),
    }
}

pub(super) fn parse_turn(provider: Provider, parsed: &Value) -> Result<ModelTurn, HandsError> {
    match provider.wire() {
        Wire::Responses => parse_responses(parsed),
        Wire::Messages => parse_messages(parsed),
        Wire::Interactions => parse_interactions(parsed),
        Wire::ChatCompletions => parse_chat_completions(parsed),
        Wire::OllamaChat => parse_ollama(parsed),
    }
}

fn parse_responses(parsed: &Value) -> Result<ModelTurn, HandsError> {
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    if let Some(output) = parsed.get("output").and_then(Value::as_array) {
        for item in output {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "function_call" {
                calls.push(fn_call_from_item(item, "call_id"));
            } else if kind == "message"
                && let Some(content) = item.get("content").and_then(Value::as_array)
            {
                for part in content {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        texts.push(t.to_string());
                    }
                }
            }
        }
    }
    if texts.is_empty()
        && let Some(t) = parsed.get("output_text").and_then(Value::as_str)
    {
        texts.push(t.to_string());
    }
    Ok(ModelTurn {
        calls,
        text: first_text(texts),
    })
}

fn parse_messages(parsed: &Value) -> Result<ModelTurn, HandsError> {
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    if let Some(content) = parsed.get("content").and_then(Value::as_array) {
        for item in content {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "tool_use" {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let call_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("call")
                    .to_string();
                let arguments = item.get("input").cloned().unwrap_or(json!({}));
                calls.push(FnCall {
                    call_id,
                    name,
                    arguments,
                });
            } else if kind == "text"
                && let Some(t) = item.get("text").and_then(Value::as_str)
            {
                texts.push(t.to_string());
            }
        }
    }
    Ok(ModelTurn {
        calls,
        text: first_text(texts),
    })
}

fn parse_interactions(parsed: &Value) -> Result<ModelTurn, HandsError> {
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    if let Some(steps) = parsed.get("steps").and_then(Value::as_array) {
        for item in steps {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "function_call" {
                calls.push(fn_call_from_item(item, "id"));
            } else if let Some(t) = item.get("text").and_then(Value::as_str) {
                texts.push(t.to_string());
            } else if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        texts.push(t.to_string());
                    }
                }
            }
        }
    }
    if texts.is_empty()
        && let Some(t) = parsed.get("output_text").and_then(Value::as_str)
    {
        texts.push(t.to_string());
    }
    Ok(ModelTurn {
        calls,
        text: first_text(texts),
    })
}

fn parse_chat_completions(parsed: &Value) -> Result<ModelTurn, HandsError> {
    let message = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"));
    parse_tool_message(message)
}

fn parse_ollama(parsed: &Value) -> Result<ModelTurn, HandsError> {
    parse_tool_message(parsed.get("message"))
}

fn parse_tool_message(message: Option<&Value>) -> Result<ModelTurn, HandsError> {
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    let Some(message) = message else {
        return Ok(ModelTurn { calls, text: None });
    };
    if let Some(t) = message.get("content").and_then(Value::as_str) {
        texts.push(t.to_string());
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (i, item) in tool_calls.iter().enumerate() {
            let function = item.get("function").unwrap_or(item);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let call_id = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{i}"));
            let arguments = function.get("arguments").cloned().unwrap_or(json!({}));
            calls.push(FnCall {
                call_id,
                name,
                arguments,
            });
        }
    }
    Ok(ModelTurn {
        calls,
        text: first_text(texts),
    })
}

fn fn_call_from_item(item: &Value, id_key: &str) -> FnCall {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let call_id = item
        .get(id_key)
        .or_else(|| item.get("call_id"))
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("call")
        .to_string();
    let arguments = item.get("arguments").cloned().unwrap_or(json!({}));
    FnCall {
        call_id,
        name,
        arguments,
    }
}

fn first_text(texts: Vec<String>) -> Option<String> {
    texts.into_iter().find(|s| !s.trim().is_empty())
}
