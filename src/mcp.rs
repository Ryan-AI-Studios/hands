use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::stdio;
use rmcp::{ServiceExt, schemars, tool, tool_router};

use crate::actuate::{self, ActuateRequest};
use crate::allows;
use crate::attach;
use crate::error::HandsError;
use crate::extract::Detail;
use crate::fence;
use crate::lease;
use crate::logs;
use crate::observe::{ObserveRequest, observe, serialize_envelope};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ObserveParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub element_id: Option<String>,
    #[serde(default)]
    pub grid: Option<String>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TypeParams {
    pub text: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct KeyParams {
    pub name: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
    pub dy: i32,
    #[serde(default)]
    pub dx: Option<i32>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub element_id: Option<String>,
    #[serde(default)]
    pub grid: Option<String>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitSettleParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default)]
    pub w: Option<i32>,
    #[serde(default)]
    pub h: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StopParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AttachParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub plan: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConfirmParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub revoke: Option<bool>,
    #[serde(default)]
    pub list: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LogsParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub list: Option<bool>,
    #[serde(default)]
    pub tail: Option<u32>,
}

#[derive(Clone, Default)]
pub struct HandsServer;

#[tool_router(server_handler)]
impl HandsServer {
    #[tool(
        description = "Capture the desktop: screenshot path, 100px grid descriptor, UIA map, Chrome chr: ids when the host is connected, capped extract"
    )]
    fn observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_observe(params))
    }

    #[tool(
        description = "Bézier-move and left-click a UIA id, Chrome `chr:` id, grid cell, or pixel"
    )]
    fn click(
        &self,
        Parameters(params): Parameters<ClickParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::click(click_req(params))))
    }

    #[tool(
        description = "Bézier-move to a UIA id, Chrome `chr:` id, grid cell, or pixel and pause 100 ms (no click)"
    )]
    fn hover(
        &self,
        Parameters(params): Parameters<ClickParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::hover(click_req(params))))
    }

    #[tool(
        name = "type",
        description = "Type text: short Unicode keystrokes or long clipboard paste+restore"
    )]
    fn r#type(
        &self,
        Parameters(params): Parameters<TypeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::type_text(ActuateRequest {
            session_id: params.session_id,
            text: Some(params.text),
            ..ActuateRequest::default()
        })))
    }

    #[tool(description = "Press a named key (enter, tab, ctrl+a, …)")]
    fn key(
        &self,
        Parameters(params): Parameters<KeyParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::key(ActuateRequest {
            session_id: params.session_id,
            name: Some(params.name),
            ..ActuateRequest::default()
        })))
    }

    #[tool(
        description = "Scroll the mouse wheel (dy notches, optional dx and UIA / Chrome `chr:` / grid / pixel target)"
    )]
    fn scroll(
        &self,
        Parameters(params): Parameters<ScrollParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::scroll(ActuateRequest {
            session_id: params.session_id,
            element_id: params.element_id,
            grid: params.grid,
            x: params.x,
            y: params.y,
            dy: Some(params.dy),
            dx: params.dx,
            ..ActuateRequest::default()
        })))
    }

    #[tool(description = "Wait until an ROI stops changing (pixel delta)")]
    fn wait_settle(
        &self,
        Parameters(params): Parameters<WaitSettleParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::wait_settle(ActuateRequest {
            session_id: params.session_id,
            x: params.x,
            y: params.y,
            w: params.w,
            h: params.h,
            ..ActuateRequest::default()
        })))
    }

    #[tool(description = "Abort injected input and freeze the desk lease (same as Pause/Break)")]
    fn stop(
        &self,
        Parameters(params): Parameters<StopParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_actuate(actuate::stop(ActuateRequest {
            session_id: params.session_id,
            ..ActuateRequest::default()
        })))
    }

    #[tool(
        description = "Attach to daily Chrome if open; else launch chrome.exe with no automation flags and about:blank. Does not sideload. Does not kill Chrome."
    )]
    fn attach(
        &self,
        Parameters(params): Parameters<AttachParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_attach(params))
    }

    #[tool(
        description = "Grant, revoke, or list confirm-fence allows (once / session / persist). After a refuse, call confirm then retry."
    )]
    fn confirm(
        &self,
        Parameters(params): Parameters<ConfirmParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_confirm(params))
    }

    #[tool(
        description = "Read session JSONL audit events (tail) or list session files. Does not mint a session id."
    )]
    fn logs(
        &self,
        Parameters(params): Parameters<LogsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_logs(params))
    }
}

fn click_req(params: ClickParams) -> ActuateRequest {
    ActuateRequest {
        session_id: params.session_id,
        element_id: params.element_id,
        grid: params.grid,
        x: params.x,
        y: params.y,
        ..ActuateRequest::default()
    }
}

fn run_observe(params: ObserveParams) -> CallToolResult {
    match observe_envelope(params) {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.tool_message())]),
    }
}

fn run_actuate(result: Result<crate::actuate::ActuateEnvelope, HandsError>) -> CallToolResult {
    match result.and_then(|env| actuate::serialize_envelope(&env).map(|j| (env.ok, j))) {
        Ok((_ok, json)) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.tool_message())]),
    }
}

fn run_attach(params: AttachParams) -> CallToolResult {
    match attach::run_attach(params.session_id.as_deref(), params.plan.unwrap_or(false))
        .and_then(|env| attach::serialize_attach(&env))
    {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.tool_message())]),
    }
}

fn run_confirm(params: ConfirmParams) -> CallToolResult {
    match allows::run_confirm(
        params.session_id.as_deref(),
        params.domain.as_deref(),
        params.category.as_deref(),
        params.mode.as_deref(),
        params.revoke.unwrap_or(false),
        params.list.unwrap_or(false),
    )
    .and_then(|env| allows::serialize_confirm(&env))
    {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.tool_message())]),
    }
}

fn run_logs(params: LogsParams) -> CallToolResult {
    match logs::run_logs(
        params.session_id.as_deref(),
        params.list.unwrap_or(false),
        params.tail.map(|n| n as usize),
    )
    .and_then(|env| logs::serialize_logs(&env))
    {
        Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.tool_message())]),
    }
}

fn observe_envelope(params: ObserveParams) -> Result<String, HandsError> {
    let detail = Detail::parse_arg(params.detail.as_deref()).map_err(HandsError::Observe)?;
    let envelope = observe(ObserveRequest {
        session_id: params.session_id,
        detail,
    })?;
    serialize_envelope(&envelope)
}

pub async fn serve() -> Result<(), HandsError> {
    fence::ensure_installed();
    logs::ensure_installed();
    let _lease = lease::install()?;
    let running = HandsServer
        .serve(stdio())
        .await
        .map_err(|err| HandsError::Observe(format!("mcp serve: {err}")))?;
    running
        .waiting()
        .await
        .map_err(|err| HandsError::Observe(format!("mcp wait: {err}")))?;
    Ok(())
}
