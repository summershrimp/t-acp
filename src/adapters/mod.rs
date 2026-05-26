mod generic;
mod opencode;

use generic::GenericAdapter;
use opencode::OpencodeAdapter;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterObservation {
    pub status: InstanceStatus,
    pub ui_mode: UiMode,
    pub blocking_reason: Option<String>,
    pub current_agent: Option<String>,
    pub current_model: Option<String>,
    pub current_provider: Option<String>,
    pub current_reasoning_effort: Option<String>,
    pub current_context_window: Option<String>,
    pub current_context_usage_percent: Option<u8>,
    pub need_interactive: bool,
    pub interactive_kind: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceStatus {
    Starting,
    Ready,
    Busy,
    Blocked,
}

impl InstanceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Busy => "busy",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiMode {
    Unknown,
    Normal,
    Input,
    PermissionPrompt,
    QuestionPrompt,
    ModelPicker,
}

impl UiMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Normal => "normal",
            Self::Input => "input",
            Self::PermissionPrompt => "permission_prompt",
            Self::QuestionPrompt => "question_prompt",
            Self::ModelPicker => "model_picker",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdapterError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    UiNotDetected(String),
    #[error("{0}")]
    UnsupportedAction(String),
}

impl AdapterError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::UiNotDetected(_) => "ui_not_detected",
            Self::UnsupportedAction(_) => "unsupported_action",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::UiNotDetected(_) => 409,
            Self::UnsupportedAction(_) => 501,
        }
    }
}

pub trait Adapter: Sync {
    fn canonical_agent_kind(&self) -> &'static str;

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn name(&self) -> &'static str {
        "generic"
    }

    fn matches_command(&self, file_name: &str) -> bool {
        file_name == self.canonical_agent_kind() || self.aliases().contains(&file_name)
    }

    fn observe(&self, output_tail: &[u8]) -> AdapterObservation {
        observe_generic(output_tail)
    }

    fn send_prompt(&self, prompt: &[u8]) -> Result<Vec<u8>, AdapterError> {
        generic_send_prompt(prompt)
    }

    fn approve_permission(&self, _output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
        Ok(b"\r".to_vec())
    }

    fn reject_permission(&self, _output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
        Ok(b"\x1b".to_vec())
    }

    fn previous_model(&self) -> Result<Vec<u8>, AdapterError> {
        Err(AdapterError::UnsupportedAction(
            "previous-model needs an adapter-specific implementation".to_string(),
        ))
    }

    fn next_model(&self) -> Result<Vec<u8>, AdapterError> {
        Err(AdapterError::UnsupportedAction(
            "next-model needs an adapter-specific implementation".to_string(),
        ))
    }

    fn switch_model(&self, _body: &[u8]) -> Result<Vec<u8>, AdapterError> {
        Err(AdapterError::UnsupportedAction(
            "switch-model needs an adapter-specific implementation".to_string(),
        ))
    }
}

static OPENCODE: OpencodeAdapter = OpencodeAdapter;
static CLAUDE_CODE: GenericAdapter = GenericAdapter::new("claude_code", &["claude", "claude-code"]);
static CODEX: GenericAdapter = GenericAdapter::new("codex", &["codex"]);
static FALLBACK: GenericAdapter = GenericAdapter::new("generic", &[]);

pub fn canonical_agent_kind(command: &str) -> String {
    let file_name = command_file_name(command);
    registered_adapters()
        .into_iter()
        .find(|adapter| adapter.matches_command(file_name))
        .map(|adapter| adapter.canonical_agent_kind().to_string())
        .unwrap_or_else(|| file_name.to_string())
}

pub fn for_agent_kind(agent_kind: &str) -> &'static dyn Adapter {
    registered_adapters()
        .into_iter()
        .find(|adapter| adapter.canonical_agent_kind() == agent_kind)
        .unwrap_or(&FALLBACK)
}

fn registered_adapters() -> [&'static dyn Adapter; 3] {
    [&OPENCODE, &CLAUDE_CODE, &CODEX]
}

fn command_file_name(command: &str) -> &str {
    let file_name = command
        .rsplit_once('/')
        .map(|(_, file_name)| file_name)
        .unwrap_or(command);
    file_name.strip_suffix(".exe").unwrap_or(file_name)
}

fn starting_observation() -> AdapterObservation {
    AdapterObservation {
        status: InstanceStatus::Starting,
        ui_mode: UiMode::Unknown,
        blocking_reason: None,
        current_agent: None,
        current_model: None,
        current_provider: None,
        current_reasoning_effort: None,
        current_context_window: None,
        current_context_usage_percent: None,
        need_interactive: false,
        interactive_kind: None,
    }
}

fn observe_generic(output_tail: &[u8]) -> AdapterObservation {
    if output_tail.is_empty() {
        return starting_observation();
    }

    AdapterObservation {
        status: InstanceStatus::Ready,
        ui_mode: UiMode::Normal,
        blocking_reason: None,
        current_agent: None,
        current_model: None,
        current_provider: None,
        current_reasoning_effort: None,
        current_context_window: None,
        current_context_usage_percent: None,
        need_interactive: false,
        interactive_kind: None,
    }
}

fn generic_send_prompt(prompt: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if prompt.is_empty() {
        return Err(AdapterError::BadRequest(
            "send-prompt requires a non-empty request body".to_string(),
        ));
    }

    let mut bytes = prompt.to_vec();
    if !bytes.ends_with(b"\n") && !bytes.ends_with(b"\r") {
        bytes.push(b'\n');
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{canonical_agent_kind, for_agent_kind};

    #[test]
    fn canonicalizes_known_agent_commands() {
        assert_eq!(canonical_agent_kind("opencode"), "opencode");
        assert_eq!(canonical_agent_kind("/usr/local/bin/opencode"), "opencode");
        assert_eq!(canonical_agent_kind("claude-code"), "claude_code");
    }

    #[test]
    fn generic_adapter_names_stay_generic_for_known_kinds() {
        assert_eq!(for_agent_kind("claude_code").name(), "generic");
        assert_eq!(for_agent_kind("codex").name(), "generic");
    }
}
