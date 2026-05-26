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
    pub need_interactive: bool,
    pub interactive_kind: Option<String>,
    pub interaction_request: Option<InteractionRequest>,
    pub focused: bool,
    pub exit_status: Option<String>,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
    pub screen_tail: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentEventState {
    pub status: String,
    pub ui_mode: String,
    pub blocking_reason: Option<String>,
    pub current_agent: Option<String>,
    pub current_model: Option<String>,
    pub current_provider: Option<String>,
    pub current_reasoning_effort: Option<String>,
    pub current_context_window: Option<String>,
    pub current_context_usage_percent: Option<u8>,
    pub need_interactive: bool,
    pub interactive_kind: Option<String>,
    pub focused: bool,
    pub exit_status: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    Snapshot {
        seq: u64,
        instance_id: String,
        ts_ms: u128,
        state: AgentEventState,
    },
    StateChanged {
        seq: u64,
        instance_id: String,
        ts_ms: u128,
        changed_fields: Vec<String>,
        state: AgentEventState,
    },
    Exited {
        seq: u64,
        instance_id: String,
        ts_ms: u128,
        changed_fields: Vec<String>,
        state: AgentEventState,
    },
}

impl AgentStreamEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::Snapshot { .. } => "snapshot",
            Self::StateChanged { .. } => "state_changed",
            Self::Exited { .. } => "exited",
        }
    }

    pub fn instance_id(&self) -> &str {
        match self {
            Self::Snapshot { instance_id, .. }
            | Self::StateChanged { instance_id, .. }
            | Self::Exited { instance_id, .. } => instance_id,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::Snapshot { seq, .. }
            | Self::StateChanged { seq, .. }
            | Self::Exited { seq, .. } => *seq,
        }
    }

    pub fn event_id(&self) -> String {
        format!("{}:{}", self.instance_id(), self.seq())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct InteractionRequest {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub prompt: Option<String>,
    pub options: Vec<InteractionOption>,
    pub custom_answer_allowed: bool,
    pub confidence: u8,
    pub evidence: Vec<InteractionEvidence>,
    pub raw: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct InteractionOption {
    pub key: String,
    pub label: String,
    pub selected: bool,
    pub action: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct InteractionEvidence {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SubmitInteractionRequest {
    pub interaction_id: String,
    pub option_key: Option<String>,
    pub custom_answer: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentList {
    pub agents: Vec<AgentView>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObservationView {
    pub agent: AgentView,
    pub screen: ScreenSnapshot,
    pub events: Vec<ObservationEvent>,
    pub raw_tail_hex: String,
    pub raw_tail_utf8_lossy: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScreenSnapshot {
    pub rows: u16,
    pub cols: u16,
    pub cursor: CursorPosition,
    pub lines: Vec<String>,
    pub text: String,
    pub updated_at_ms: u128,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CursorPosition {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObservationEvent {
    pub seq: u64,
    pub at_ms: u128,
    pub kind: String,
    pub message: String,
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
