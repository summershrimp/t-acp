#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterObservation {
    pub status: InstanceStatus,
    pub ui_mode: UiMode,
    pub blocking_reason: Option<String>,
    pub current_model: Option<String>,
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
    ModelPicker,
}

impl UiMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Normal => "normal",
            Self::Input => "input",
            Self::PermissionPrompt => "permission_prompt",
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

pub fn canonical_agent_kind(command: &str) -> String {
    let file_name = command
        .rsplit_once('/')
        .map(|(_, file_name)| file_name)
        .unwrap_or(command);
    let file_name = file_name.strip_suffix(".exe").unwrap_or(file_name);

    match file_name {
        "opencode" => "opencode".to_string(),
        "claude" | "claude-code" => "claude_code".to_string(),
        "codex" => "codex".to_string(),
        other => other.to_string(),
    }
}

pub fn adapter_name(agent_kind: &str) -> &'static str {
    if agent_kind == "opencode" {
        "opencode"
    } else {
        "generic"
    }
}

pub fn observe(agent_kind: &str, output_tail: &[u8]) -> AdapterObservation {
    if agent_kind == "opencode" {
        observe_opencode(output_tail)
    } else {
        observe_generic(output_tail)
    }
}

pub fn send_prompt(agent_kind: &str, prompt: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if agent_kind == "opencode" {
        opencode_send_prompt(prompt)
    } else {
        generic_send_prompt(prompt)
    }
}

pub fn approve_permission(agent_kind: &str, output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if agent_kind == "opencode" {
        let observation = observe_opencode(output_tail);
        if observation.ui_mode != UiMode::PermissionPrompt {
            return Err(AdapterError::UiNotDetected(
                "opencode permission prompt is not visible".to_string(),
            ));
        }
    }

    Ok(b"\r".to_vec())
}

pub fn reject_permission(agent_kind: &str, output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if agent_kind == "opencode" {
        let observation = observe_opencode(output_tail);
        if observation.ui_mode != UiMode::PermissionPrompt {
            return Err(AdapterError::UiNotDetected(
                "opencode permission prompt is not visible".to_string(),
            ));
        }
    }

    Ok(b"\x1b".to_vec())
}

pub fn switch_model(agent_kind: &str, _body: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if agent_kind == "opencode" {
        Err(AdapterError::UnsupportedAction(
            "opencode runtime model switching needs a stable TUI shortcut or command before it can be automated safely".to_string(),
        ))
    } else {
        Err(AdapterError::UnsupportedAction(
            "switch-model needs an adapter-specific implementation".to_string(),
        ))
    }
}

fn observe_opencode(output_tail: &[u8]) -> AdapterObservation {
    if output_tail.is_empty() {
        return AdapterObservation {
            status: InstanceStatus::Starting,
            ui_mode: UiMode::Unknown,
            blocking_reason: None,
            current_model: None,
        };
    }

    let plain = strip_ansi(&String::from_utf8_lossy(output_tail));
    let lower = plain.to_ascii_lowercase();

    if looks_like_permission_prompt(&lower) {
        return AdapterObservation {
            status: InstanceStatus::Blocked,
            ui_mode: UiMode::PermissionPrompt,
            blocking_reason: Some("permission".to_string()),
            current_model: extract_model(&plain),
        };
    }

    if looks_like_model_picker(&lower) {
        return AdapterObservation {
            status: InstanceStatus::Ready,
            ui_mode: UiMode::ModelPicker,
            blocking_reason: None,
            current_model: extract_model(&plain),
        };
    }

    if looks_busy(&lower) {
        return AdapterObservation {
            status: InstanceStatus::Busy,
            ui_mode: UiMode::Normal,
            blocking_reason: None,
            current_model: extract_model(&plain),
        };
    }

    AdapterObservation {
        status: InstanceStatus::Ready,
        ui_mode: UiMode::Input,
        blocking_reason: None,
        current_model: extract_model(&plain),
    }
}

fn observe_generic(output_tail: &[u8]) -> AdapterObservation {
    if output_tail.is_empty() {
        return AdapterObservation {
            status: InstanceStatus::Starting,
            ui_mode: UiMode::Unknown,
            blocking_reason: None,
            current_model: None,
        };
    }

    AdapterObservation {
        status: InstanceStatus::Ready,
        ui_mode: UiMode::Normal,
        blocking_reason: None,
        current_model: None,
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

fn opencode_send_prompt(prompt: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if prompt.is_empty() {
        return Err(AdapterError::BadRequest(
            "send-prompt requires a non-empty request body".to_string(),
        ));
    }

    let mut bytes = Vec::with_capacity(prompt.len() + 16);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(prompt);
    bytes.extend_from_slice(b"\x1b[201~\r");
    Ok(bytes)
}

fn looks_like_permission_prompt(lower: &str) -> bool {
    let has_permission_word = lower.contains("permission")
        || lower.contains("approve")
        || lower.contains("approval")
        || lower.contains("allow")
        || lower.contains("deny");
    let has_action_word = lower.contains("tool")
        || lower.contains("command")
        || lower.contains("execute")
        || lower.contains("run")
        || lower.contains("edit")
        || lower.contains("write");
    let has_choice_pair = (lower.contains("allow") || lower.contains("approve"))
        && (lower.contains("deny") || lower.contains("reject"));

    (has_permission_word && has_action_word) || has_choice_pair
}

fn looks_like_model_picker(lower: &str) -> bool {
    (lower.contains("model") && lower.contains("provider"))
        || lower.contains("select model")
        || lower.contains("choose model")
}

fn looks_busy(lower: &str) -> bool {
    lower.contains("thinking")
        || lower.contains("working")
        || lower.contains("running")
        || lower.contains("processing")
}

fn extract_model(plain: &str) -> Option<String> {
    plain.lines().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        let (_, value) = trimmed.split_once(':')?;

        if lower.starts_with("model:") {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        } else {
            None
        }
    })
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut prev_was_escape = false;
                for next in chars.by_ref() {
                    if next == '\x07' || (prev_was_escape && next == '\\') {
                        break;
                    }
                    prev_was_escape = next == '\x1b';
                }
            }
            _ => {}
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::{
        AdapterError, InstanceStatus, UiMode, approve_permission, canonical_agent_kind, observe,
        send_prompt, switch_model,
    };

    #[test]
    fn canonicalizes_known_agent_commands() {
        assert_eq!(canonical_agent_kind("opencode"), "opencode");
        assert_eq!(canonical_agent_kind("/usr/local/bin/opencode"), "opencode");
        assert_eq!(canonical_agent_kind("claude-code"), "claude_code");
    }

    #[test]
    fn opencode_send_prompt_uses_bracketed_paste_and_submit() {
        let bytes = send_prompt("opencode", b"hello\nworld").unwrap();
        assert_eq!(bytes, b"\x1b[200~hello\nworld\x1b[201~\r");
    }

    #[test]
    fn opencode_detects_permission_prompt() {
        let observation = observe("opencode", b"\x1b[1mAllow command to run?\x1b[0m Deny");
        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert_eq!(observation.ui_mode, UiMode::PermissionPrompt);
        assert_eq!(observation.blocking_reason.as_deref(), Some("permission"));
    }

    #[test]
    fn opencode_permission_actions_require_visible_prompt() {
        let error = approve_permission("opencode", b"ready").unwrap_err();
        assert_eq!(
            error,
            AdapterError::UiNotDetected("opencode permission prompt is not visible".to_string())
        );
    }

    #[test]
    fn opencode_switch_model_is_explicitly_unsupported_for_now() {
        let error = switch_model("opencode", b"anthropic/claude").unwrap_err();
        assert_eq!(error.code(), "unsupported_action");
    }
}
use thiserror::Error;
