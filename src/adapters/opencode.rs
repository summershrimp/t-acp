use super::{
    Adapter, AdapterError, AdapterObservation, InstanceStatus, UiMode, starting_observation,
};

const PERMISSION_PROMPT_NOT_VISIBLE: &str = "opencode permission prompt is not visible";
const KNOWN_PROVIDERS: &[&str] = &[
    "GitHub Copilot",
    "Azure OpenAI",
    "AWS Bedrock",
    "Anthropic",
    "OpenAI",
    "OpenRouter",
    "Google",
    "Vertex AI",
    "Together AI",
    "Mistral",
    "DeepSeek",
    "Moonshot",
    "Alibaba",
    "Groq",
    "xAI",
];

pub struct OpencodeAdapter;

#[derive(Default)]
struct RuntimeMetadata {
    current_agent: Option<String>,
    current_model: Option<String>,
    current_provider: Option<String>,
    current_reasoning_effort: Option<String>,
    current_context_window: Option<String>,
    current_context_usage_percent: Option<u8>,
}

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
    let metadata = extract_runtime_metadata(&plain);

    if looks_like_permission_prompt(&lower) {
        return build_observation(
            InstanceStatus::Blocked,
            UiMode::PermissionPrompt,
            Some("permission"),
            &metadata,
        );
    }

    if looks_like_model_picker(&lower) {
        return build_observation(InstanceStatus::Ready, UiMode::ModelPicker, None, &metadata);
    }

    if looks_busy(&lower) {
        return build_observation(InstanceStatus::Busy, UiMode::Normal, None, &metadata);
    }

    build_observation(InstanceStatus::Ready, UiMode::Input, None, &metadata)
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

fn build_observation(
    status: InstanceStatus,
    ui_mode: UiMode,
    blocking_reason: Option<&str>,
    metadata: &RuntimeMetadata,
) -> AdapterObservation {
    AdapterObservation {
        status,
        ui_mode,
        blocking_reason: blocking_reason.map(str::to_string),
        current_agent: metadata.current_agent.clone(),
        current_model: metadata.current_model.clone(),
        current_provider: metadata.current_provider.clone(),
        current_reasoning_effort: metadata.current_reasoning_effort.clone(),
        current_context_window: metadata.current_context_window.clone(),
        current_context_usage_percent: metadata.current_context_usage_percent,
    }
}

fn extract_runtime_metadata(plain: &str) -> RuntimeMetadata {
    let mut metadata = plain
        .lines()
        .rev()
        .find_map(parse_runtime_footer)
        .unwrap_or_default();

    if metadata.current_model.is_none() {
        metadata.current_model = extract_labeled_model(plain);
    }

    if let Some((context_window, context_usage_percent)) = extract_context_usage(plain) {
        metadata.current_context_window = Some(context_window);
        metadata.current_context_usage_percent = Some(context_usage_percent);
    }

    metadata
}

fn parse_runtime_footer(line: &str) -> Option<RuntimeMetadata> {
    if !line.contains('·') {
        return None;
    }

    let trimmed = line
        .trim()
        .trim_start_matches(|ch: char| matches!(ch, '┃' | '│' | '┆' | '┇' | '┊' | '┋' | '¦' | '|'))
        .trim();
    let parts: Vec<&str> = trimmed
        .split('·')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();

    if parts.len() < 2 {
        return None;
    }

    let current_agent = Some(parts[0].to_string());
    let (model_provider_segment, current_reasoning_effort) =
        if looks_like_reasoning_effort(parts[parts.len() - 1]) {
            (
                &parts[1..parts.len() - 1],
                Some(parts[parts.len() - 1].to_string()),
            )
        } else {
            (&parts[1..], None)
        };

    if model_provider_segment.is_empty() {
        return None;
    }

    let (current_model, current_provider) = if model_provider_segment.len() == 1 {
        split_model_and_provider(model_provider_segment[0])
    } else {
        (
            Some(model_provider_segment[0].to_string()),
            Some(model_provider_segment[1..].join(" · ")),
        )
    };

    Some(RuntimeMetadata {
        current_agent,
        current_model,
        current_provider,
        current_reasoning_effort,
        current_context_window: None,
        current_context_usage_percent: None,
    })
}

fn looks_like_reasoning_effort(part: &str) -> bool {
    matches!(
        part.trim().to_ascii_lowercase().as_str(),
        "low" | "medium" | "high" | "xhigh"
    )
}

fn split_model_and_provider(segment: &str) -> (Option<String>, Option<String>) {
    for provider in KNOWN_PROVIDERS {
        if let Some(model) = segment.strip_suffix(provider) {
            let model = model.trim();
            if !model.is_empty() {
                return (Some(model.to_string()), Some((*provider).to_string()));
            }
        }
    }

    (Some(segment.to_string()), None)
}

fn extract_labeled_model(plain: &str) -> Option<String> {
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

fn extract_context_usage(plain: &str) -> Option<(String, u8)> {
    plain.lines().rev().find_map(parse_context_usage_line)
}

fn parse_context_usage_line(line: &str) -> Option<(String, u8)> {
    let trimmed = line.trim();
    let open_paren = trimmed.find("(")?;
    let percent_start = open_paren + 1;
    let percent_end = trimmed[percent_start..].find("%)")? + percent_start;
    let context_window = trimmed[..open_paren].trim();

    if context_window.is_empty() {
        return None;
    }

    let context_usage_percent = trimmed[percent_start..percent_end].trim().parse().ok()?;
    Some((context_window.to_string(), context_usage_percent))
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
    fn extracts_runtime_footer_metadata() {
        let observation = OpencodeAdapter
            .observe("Prompt\n┃  Build · GPT-5.4 GitHub Copilot · high\n".as_bytes());

        assert_eq!(observation.current_agent.as_deref(), Some("Build"));
        assert_eq!(observation.current_model.as_deref(), Some("GPT-5.4"));
        assert_eq!(
            observation.current_provider.as_deref(),
            Some("GitHub Copilot")
        );
        assert_eq!(
            observation.current_reasoning_effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn falls_back_to_labeled_model_when_footer_is_missing() {
        let observation = OpencodeAdapter.observe(b"Model: gpt-5.4\nReady");

        assert_eq!(observation.current_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(observation.current_agent, None);
        assert_eq!(observation.current_provider, None);
    }

    #[test]
    fn extracts_runtime_footer_without_reasoning_effort() {
        let observation =
            OpencodeAdapter.observe("Prompt\n┃  Build · GPT-5.4 GitHub Copilot\n".as_bytes());

        assert_eq!(observation.current_agent.as_deref(), Some("Build"));
        assert_eq!(observation.current_model.as_deref(), Some("GPT-5.4"));
        assert_eq!(
            observation.current_provider.as_deref(),
            Some("GitHub Copilot")
        );
        assert_eq!(observation.current_reasoning_effort, None);
    }

    #[test]
    fn extracts_context_usage_metadata() {
        let observation =
            OpencodeAdapter.observe("Prompt\n42.6K (21%)  ctrl+p commands\n".as_bytes());

        assert_eq!(observation.current_context_window.as_deref(), Some("42.6K"));
        assert_eq!(observation.current_context_usage_percent, Some(21));
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
