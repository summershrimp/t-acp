use super::{
    Adapter, AdapterError, AdapterObservation, InstanceStatus, UiMode, starting_observation,
};

const PERMISSION_PROMPT_NOT_VISIBLE: &str = "opencode permission prompt is not visible";

pub struct OpencodeAdapter;

impl Adapter for OpencodeAdapter {
    fn canonical_agent_kind(&self) -> &'static str {
        "opencode"
    }

    fn name(&self) -> &'static str {
        "opencode"
    }

    fn observe(&self, output_tail: &[u8]) -> AdapterObservation {
        observe_opencode(output_tail)
    }

    fn send_prompt(&self, prompt: &[u8]) -> Result<Vec<u8>, AdapterError> {
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

    fn approve_permission(&self, output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
        require_permission_prompt(output_tail)?;
        Ok(b"\r".to_vec())
    }

    fn reject_permission(&self, output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
        require_permission_prompt(output_tail)?;
        Ok(b"\x1b".to_vec())
    }

    fn switch_model(&self, _body: &[u8]) -> Result<Vec<u8>, AdapterError> {
        Err(AdapterError::UnsupportedAction(
            "opencode runtime model switching needs a stable TUI shortcut or command before it can be automated safely".to_string(),
        ))
    }
}

fn observe_opencode(output_tail: &[u8]) -> AdapterObservation {
    if output_tail.is_empty() {
        return starting_observation();
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

fn require_permission_prompt(output_tail: &[u8]) -> Result<(), AdapterError> {
    if observe_opencode(output_tail).ui_mode == UiMode::PermissionPrompt {
        Ok(())
    } else {
        Err(AdapterError::UiNotDetected(
            PERMISSION_PROMPT_NOT_VISIBLE.to_string(),
        ))
    }
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
    use super::*;

    #[test]
    fn send_prompt_uses_bracketed_paste_and_submit() {
        let bytes = OpencodeAdapter.send_prompt(b"hello\nworld").unwrap();
        assert_eq!(bytes, b"\x1b[200~hello\nworld\x1b[201~\r");
    }

    #[test]
    fn detects_permission_prompt() {
        let observation = OpencodeAdapter.observe(b"\x1b[1mAllow command to run?\x1b[0m Deny");
        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert_eq!(observation.ui_mode, UiMode::PermissionPrompt);
        assert_eq!(observation.blocking_reason.as_deref(), Some("permission"));
    }

    #[test]
    fn permission_actions_require_visible_prompt() {
        let error = OpencodeAdapter.approve_permission(b"ready").unwrap_err();
        assert_eq!(
            error,
            AdapterError::UiNotDetected(PERMISSION_PROMPT_NOT_VISIBLE.to_string())
        );
    }

    #[test]
    fn switch_model_is_explicitly_unsupported_for_now() {
        let error = OpencodeAdapter
            .switch_model(b"anthropic/claude")
            .unwrap_err();
        assert_eq!(error.code(), "unsupported_action");
    }
}
