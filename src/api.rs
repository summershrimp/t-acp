use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentView {
    pub id: String,
    pub agent_kind: String,
    pub adapter: String,
    pub pid: Option<u32>,
    pub cwd: String,
    pub command: String,
    pub status: String,
    pub ui_mode: String,
    pub blocking_reason: Option<String>,
    pub current_agent: Option<String>,
    pub current_model: Option<String>,
    pub current_provider: Option<String>,
    pub current_reasoning_effort: Option<String>,
    pub current_context_window: Option<String>,
    pub current_context_usage_percent: Option<u8>,
    pub focused: bool,
    pub exit_status: Option<String>,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
    pub screen_tail: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentList {
    pub agents: Vec<AgentView>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterAgentRequest {
    pub id: String,
    pub agent_kind: String,
    pub pid: Option<u32>,
    pub cwd: String,
    pub command: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExitRequest {
    pub status: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ActionQueued {
    pub queued: bool,
    pub adapter: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ErrorBody {
    pub error: String,
    pub message: String,
}
