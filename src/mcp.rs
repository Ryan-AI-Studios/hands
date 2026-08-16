use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::stdio;
use rmcp::{ServiceExt, schemars, tool, tool_router};

use crate::error::HandsError;
use crate::extract::Detail;
use crate::observe::{ObserveRequest, observe, serialize_envelope};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ObserveParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Default)]
pub struct HandsServer;

#[tool_router(server_handler)]
impl HandsServer {
    #[tool(
        description = "Capture the desktop: screenshot path, 100px grid descriptor, UIA map, capped extract"
    )]
    fn observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(run_observe(params))
    }
}

fn run_observe(params: ObserveParams) -> CallToolResult {
    match observe_envelope(params) {
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
