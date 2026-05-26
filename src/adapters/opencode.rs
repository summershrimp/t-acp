use super::{
    Adapter, AdapterError, AdapterObservation, InstanceStatus, UiMode, starting_observation,
};
use crate::api::{InteractionEvidence, InteractionOption, InteractionRequest};
use crate::interactions::{InteractionSubmission, push_evidence, with_stable_id};

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedPermission {
    title: Option<String>,
    subject: Option<String>,
    question: Option<String>,
    options: Vec<ParsedOption>,
    raw: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedOption {
    key: String,
    label: String,
    selected: bool,
    decision: String,
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

    fn submit_interaction(
        &self,
        interaction: &InteractionRequest,
        submission: &InteractionSubmission,
    ) -> Result<Vec<u8>, AdapterError> {
        submit_opencode_interaction(interaction, submission)
    }

    fn approve_permission(&self, output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
        require_permission_prompt(output_tail)?;
        Ok(b"\r".to_vec())
    }

    fn reject_permission(&self, output_tail: &[u8]) -> Result<Vec<u8>, AdapterError> {
        require_permission_prompt(output_tail)?;
        Ok(b"\x1b".to_vec())
    }

    fn previous_model(&self) -> Result<Vec<u8>, AdapterError> {
        Ok(b"\x1b[1;2Q".to_vec())
    }

    fn next_model(&self) -> Result<Vec<u8>, AdapterError> {
        Ok(b"\x1bOQ".to_vec())
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
    let permission = parse_permission(&plain);

    if permission.is_some() || looks_like_permission_prompt(&lower) {
        let interaction_request = permission
            .as_ref()
            .map(|permission| permission_to_interaction(&plain, permission))
            .or_else(|| Some(permission_interaction_from_plain(&plain)));
        return build_observation(
            InstanceStatus::Blocked,
            UiMode::PermissionPrompt,
            Some("permission"),
            &metadata,
            interaction_request,
        );
    }

    if let Some(question_request) = parse_question_request(&plain) {
        return build_observation(
            InstanceStatus::Blocked,
            UiMode::QuestionPrompt,
            Some("question"),
            &metadata,
            Some(question_request),
        );
    }

    if looks_like_question_prompt(&lower) {
        return build_observation(
            InstanceStatus::Blocked,
            UiMode::QuestionPrompt,
            Some("question"),
            &metadata,
            None,
        );
    }

    if looks_like_model_picker(&lower) {
        return build_observation(
            InstanceStatus::Ready,
            UiMode::ModelPicker,
            None,
            &metadata,
            None,
        );
    }

    if looks_busy(&lower) {
        return build_observation(InstanceStatus::Busy, UiMode::Normal, None, &metadata, None);
    }

    build_observation(InstanceStatus::Ready, UiMode::Input, None, &metadata, None)
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
    let has_prompt_marker = lower.contains("permission required")
        || lower.contains("permission request")
        || lower.contains("requires permission")
        || lower.contains("approval required")
        || lower.contains("do you want to proceed")
        || lower.contains("access external directory")
        || lower.contains("access external file")
        || lower.contains("access outside");
    let has_permission_word =
        lower.contains("permission") || lower.contains("approve") || lower.contains("approval");
    let has_action_word = lower.contains("tool")
        || lower.contains("command")
        || lower.contains("access")
        || lower.contains("external directory")
        || lower.contains("execute")
        || lower.contains("run");
    let has_choice_pair = (lower.contains("allow") || lower.contains("approve"))
        && (lower.contains("deny") || lower.contains("reject"));

    has_prompt_marker
        || (has_action_word && has_choice_pair)
        || (has_permission_word && has_choice_pair)
}

fn parse_permission(plain: &str) -> Option<ParsedPermission> {
    let lines = clean_lines(plain);
    if lines.is_empty() {
        return None;
    }

    let options = parse_options(&lines);
    let question = find_question(&lines);
    let lower = plain.to_ascii_lowercase();
    let has_decision_option = options
        .iter()
        .any(|option| option.decision.as_str() != "unknown");
    let has_explicit_prompt = lower.contains("permission")
        || lower.contains("approval")
        || lower.contains("approve")
        || lower.contains("deny")
        || lower.contains("reject");
    let has_permission_context =
        looks_like_permission_prompt(&lower) || lower.contains("do you want to proceed");

    if options.is_empty() && !has_permission_context {
        return None;
    }

    let has_explicit_decision_prompt =
        has_explicit_prompt && question.is_some() && has_decision_option;
    let has_button_decisions = has_permission_context && has_decision_option;
    let has_context_without_options = has_permission_context && options.is_empty();
    if !has_button_decisions && !has_context_without_options && !has_explicit_decision_prompt {
        return None;
    }

    let title = find_title(&lines);
    let subject = find_subject(&lines, title.as_deref(), question.as_deref());
    let raw = relevant_raw_block(&lines);

    Some(ParsedPermission {
        title,
        subject,
        question,
        options,
        raw,
    })
}

fn permission_to_interaction(plain: &str, permission: &ParsedPermission) -> InteractionRequest {
    let kind = classify_permission_interaction_kind(plain).to_string();
    with_stable_id(InteractionRequest {
        id: String::new(),
        kind: kind.clone(),
        source: "opencode".to_string(),
        title: permission.title.clone(),
        subject: permission.subject.clone(),
        prompt: permission.question.clone(),
        options: permission
            .options
            .iter()
            .filter(|option| option.decision != "unknown")
            .map(|option| InteractionOption {
                key: option.key.clone(),
                label: option.label.clone(),
                selected: option.selected,
                action: Some(option.decision.clone()),
            })
            .collect(),
        custom_answer_allowed: false,
        confidence: permission_confidence(permission.options.len(), permission.subject.is_some()),
        evidence: permission_evidence(
            &kind,
            permission.title.as_deref(),
            permission.subject.as_deref(),
            permission.question.as_deref(),
            &permission.options,
            &clean_lines(plain),
        ),
        raw: permission.raw.clone(),
    })
}

fn permission_interaction_from_plain(plain: &str) -> InteractionRequest {
    let lines = clean_lines(plain);
    let title = find_title(&lines);
    let prompt = find_question(&lines);
    let subject = find_subject(&lines, None, prompt.as_deref());
    let options = parse_options(&lines);
    let kind = classify_permission_interaction_kind(plain).to_string();
    with_stable_id(InteractionRequest {
        id: String::new(),
        kind: kind.clone(),
        source: "opencode".to_string(),
        title: title.clone(),
        subject: subject.clone(),
        prompt: prompt.clone(),
        options: options
            .iter()
            .filter(|option| option.decision != "unknown")
            .map(|option| InteractionOption {
                key: option.key.clone(),
                label: option.label.clone(),
                selected: option.selected,
                action: Some(option.decision.clone()),
            })
            .collect(),
        custom_answer_allowed: false,
        confidence: permission_confidence(options.len(), subject.is_some()),
        evidence: permission_evidence(
            &kind,
            title.as_deref(),
            subject.as_deref(),
            prompt.as_deref(),
            &options,
            &lines,
        ),
        raw: relevant_raw_block(&lines),
    })
}

fn classify_permission_interaction_kind(plain: &str) -> &'static str {
    let lower = plain.to_ascii_lowercase();
    if lower.contains("external_directory")
        || lower.contains("external directory")
        || (lower.contains("outside")
            && (lower.contains("working directory")
                || lower.contains("workspace")
                || lower.contains("project")))
    {
        return "external_directory";
    }

    if lower.contains("doom_loop")
        || lower.contains("doom loop")
        || (lower.contains("same tool") && lower.contains("repeat"))
        || (lower.contains("repeated") && lower.contains("tool"))
    {
        return "doom_loop";
    }

    "permission"
}

fn permission_confidence(option_count: usize, has_subject: bool) -> u8 {
    match (option_count >= 2, has_subject) {
        (true, true) => 95,
        (true, false) => 88,
        (false, true) => 74,
        (false, false) => 65,
    }
}

fn permission_evidence(
    kind: &str,
    title: Option<&str>,
    subject: Option<&str>,
    prompt: Option<&str>,
    options: &[ParsedOption],
    lines: &[String],
) -> Vec<InteractionEvidence> {
    let mut evidence = Vec::new();
    push_evidence(&mut evidence, "source", Some("pty_screen"));
    push_evidence(&mut evidence, "permission_kind", Some(kind));
    push_evidence(&mut evidence, "title", title);
    push_evidence(&mut evidence, "subject", subject);
    push_evidence(&mut evidence, "prompt", prompt);
    if !options.is_empty() {
        let labels = options
            .iter()
            .map(|option| format!("{}:{}", option.key, option.label))
            .collect::<Vec<_>>()
            .join(", ");
        push_evidence(&mut evidence, "options", Some(&labels));
    }
    if let Some(patterns) = extract_patterns(lines) {
        push_evidence(&mut evidence, "patterns", Some(&patterns));
    }
    if let Some(buttons) = lines
        .iter()
        .find(|line| line.contains("Allow") && (line.contains("Reject") || line.contains("Deny")))
    {
        push_evidence(&mut evidence, "button_row", Some(buttons));
    }
    evidence
}

fn question_evidence(
    title: Option<&str>,
    prompt: Option<&str>,
    custom_answer_allowed: bool,
) -> Vec<InteractionEvidence> {
    let mut evidence = Vec::new();
    push_evidence(&mut evidence, "source", Some("pty_screen"));
    push_evidence(&mut evidence, "title", title);
    push_evidence(&mut evidence, "prompt", prompt);
    if custom_answer_allowed {
        push_evidence(&mut evidence, "custom_answer", Some("allowed"));
    }
    evidence
}

fn extract_patterns(lines: &[String]) -> Option<String> {
    let mut found_header = false;
    let patterns = lines
        .iter()
        .filter_map(|line| {
            if line.eq_ignore_ascii_case("patterns") {
                found_header = true;
                return None;
            }
            if found_header {
                return line.strip_prefix("- ").map(str::trim);
            }
            None
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if patterns.is_empty() {
        None
    } else {
        Some(patterns.join(", "))
    }
}

fn submit_opencode_interaction(
    interaction: &InteractionRequest,
    submission: &InteractionSubmission,
) -> Result<Vec<u8>, AdapterError> {
    if let InteractionSubmission::CustomAnswer { answer } = submission {
        return if interaction.kind == "question" && interaction.custom_answer_allowed {
            Ok(format!("{answer}\r").into_bytes())
        } else {
            Err(AdapterError::BadRequest(
                "custom answer is not allowed for this interaction".to_string(),
            ))
        };
    }

    let option_key = submission.option_key().ok_or_else(|| {
        AdapterError::BadRequest("interaction option_key is required".to_string())
    })?;
    let option = interaction
        .options
        .iter()
        .find(|option| option.key == option_key || option.action.as_deref() == Some(option_key))
        .ok_or_else(|| {
            AdapterError::BadRequest(format!("unknown interaction option {option_key}"))
        })?;

    match interaction.kind.as_str() {
        "permission" | "external_directory" | "doom_loop" => match option.action.as_deref() {
            Some("allow_once") => Ok(b"\r".to_vec()),
            Some("allow_persist") => Ok(b"\t\r".to_vec()),
            Some("deny") => Ok(b"\x1b".to_vec()),
            _ => Err(AdapterError::BadRequest(format!(
                "unsupported permission action for option {option_key}"
            ))),
        },
        "question" => {
            if option.key.is_empty() {
                Err(AdapterError::BadRequest(
                    "question option key must not be empty".to_string(),
                ))
            } else {
                Ok(format!("{}\r", option.key).into_bytes())
            }
        }
        other => Err(AdapterError::UnsupportedAction(format!(
            "opencode interaction kind {other} is not supported"
        ))),
    }
}

fn parse_question_request(plain: &str) -> Option<InteractionRequest> {
    let lower = plain.to_ascii_lowercase();
    if looks_like_permission_prompt(&lower) || looks_like_model_picker(&lower) {
        return None;
    }

    let lines = clean_lines(plain);
    if lines.is_empty() {
        return None;
    }

    let options = parse_options(&lines);
    if options.is_empty() {
        return None;
    }

    let prompt = find_question(&lines).or_else(|| find_question_prompt_before_options(&lines));
    let has_question_context = lower.contains("question")
        || lower.contains("custom answer")
        || lower.contains("type a custom")
        || lower.contains("select an option")
        || lower.contains("submit all")
        || lower.contains("answer");

    if prompt.is_none() || !has_question_context {
        return None;
    }

    let title = find_question_title(&lines, prompt.as_deref());
    let custom_answer_allowed = lower.contains("custom answer") || lower.contains("type a custom");
    Some(with_stable_id(InteractionRequest {
        id: String::new(),
        kind: "question".to_string(),
        source: "opencode".to_string(),
        title: title.clone(),
        subject: None,
        prompt: prompt.clone(),
        options: options
            .into_iter()
            .map(|option| InteractionOption {
                key: option.key,
                label: option.label,
                selected: option.selected,
                action: None,
            })
            .collect(),
        custom_answer_allowed,
        confidence: 90,
        evidence: question_evidence(title.as_deref(), prompt.as_deref(), custom_answer_allowed),
        raw: relevant_raw_block(&lines),
    }))
}

fn clean_lines(plain: &str) -> Vec<String> {
    plain
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn parse_options(lines: &[String]) -> Vec<ParsedOption> {
    lines
        .iter()
        .flat_map(|line| parse_option_lines(line))
        .collect()
}

fn parse_option_line(line: &str) -> Option<ParsedOption> {
    parse_option_lines(line).into_iter().next()
}

fn parse_option_lines(line: &str) -> Vec<ParsedOption> {
    let (selected, rest) = strip_selection_marker(line.trim());
    if let Some((key, label)) = parse_numbered_label(rest) {
        let decision = classify_decision(&label);
        return vec![ParsedOption {
            key,
            label,
            selected,
            decision,
        }];
    }

    parse_button_options(rest, selected)
}

fn parse_button_options(line: &str, has_selection_marker: bool) -> Vec<ParsedOption> {
    let lower = line.to_ascii_lowercase();
    let mut options = Vec::new();

    for (phrase, key, label, decision) in [
        ("allow once", "allow_once", "Allow once", "allow_once"),
        (
            "allow always",
            "allow_persist",
            "Allow always",
            "allow_persist",
        ),
        ("reject", "deny", "Reject", "deny"),
        ("deny", "deny", "Deny", "deny"),
    ] {
        if lower.contains(phrase)
            && !options
                .iter()
                .any(|option: &ParsedOption| option.key == key)
        {
            options.push(ParsedOption {
                key: key.to_string(),
                label: label.to_string(),
                selected: has_selection_marker && options.is_empty(),
                decision: decision.to_string(),
            });
        }
    }

    if lower.contains("allow always")
        && lower.contains("reject")
        && !options.iter().any(|option| option.key == "allow_once")
    {
        options.insert(
            0,
            ParsedOption {
                key: "allow_once".to_string(),
                label: "Allow once".to_string(),
                selected: has_selection_marker,
                decision: "allow_once".to_string(),
            },
        );
    }

    options
}

fn strip_selection_marker(line: &str) -> (bool, &str) {
    let trimmed = line.trim_start();
    for marker in ["❯", ">", "›", "▸", "•", "*"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return (true, rest.trim_start());
        }
    }
    (false, trimmed)
}

fn parse_numbered_label(line: &str) -> Option<(String, String)> {
    let (key, label) = line.split_once(". ")?;
    let key = key.trim();
    let label = label.trim();
    if key.is_empty()
        || label.is_empty()
        || key.len() > 3
        || !key.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some((key.to_string(), label.to_string()))
}

fn classify_decision(label: &str) -> String {
    let lower = label.to_ascii_lowercase();
    if lower == "no"
        || lower.starts_with("no,")
        || lower.contains("deny")
        || lower.contains("reject")
        || lower.contains("cancel")
    {
        return "deny".to_string();
    }

    if lower.contains("always allow")
        || lower.contains("allow access")
        || lower.contains("remember")
        || lower.contains("for this project")
        || lower.contains("for this session")
    {
        return "allow_persist".to_string();
    }

    if lower == "yes"
        || lower.starts_with("yes,")
        || lower.contains("allow")
        || lower.contains("approve")
    {
        return "allow_once".to_string();
    }

    "unknown".to_string()
}

fn find_question(lines: &[String]) -> Option<String> {
    lines.iter().rev().find(|line| line.ends_with('?')).cloned()
}

fn find_question_prompt_before_options(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .skip_while(|line| parse_option_line(line).is_some() || is_hint_line(line))
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            parse_option_line(line).is_none()
                && !is_hint_line(line)
                && !lower.contains("question")
                && line.len() > 8
        })
        .cloned()
}

fn find_question_title(lines: &[String], prompt: Option<&str>) -> Option<String> {
    lines.iter().find_map(|line| {
        if Some(line.as_str()) == prompt || parse_option_line(line).is_some() {
            return None;
        }

        let lower = line.to_ascii_lowercase();
        if lower.contains("question") || lower.contains("clarification") {
            Some(line.clone())
        } else {
            None
        }
    })
}

fn find_title(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let is_title = lower.contains("permission")
            || lower.contains("approval")
            || lower.contains("tool")
            || lower.contains("command")
            || lower.contains("bash");
        if is_title && !line.ends_with('?') && parse_option_line(line).is_none() {
            Some(line.clone())
        } else {
            None
        }
    })
}

fn find_subject(lines: &[String], title: Option<&str>, question: Option<&str>) -> Option<String> {
    lines.iter().find_map(|line| {
        if Some(line.as_str()) == title || Some(line.as_str()) == question {
            return None;
        }
        if parse_option_line(line).is_some() || is_hint_line(line) {
            return None;
        }

        let lower = normalized_line_for_matching(line);
        let looks_subject = lower.starts_with("command:")
            || lower.starts_with("tool:")
            || lower.starts_with("access external directory")
            || lower.starts_with("access external file")
            || lower.starts_with("access outside")
            || lower.starts_with("bash(")
            || lower.starts_with("edit(")
            || lower.starts_with("write(")
            || lower.starts_with("rm ")
            || lower.starts_with("rm\t")
            || lower.starts_with("rm -")
            || lower.starts_with("git ")
            || lower.starts_with("python ")
            || lower.starts_with("python3 ")
            || lower.starts_with("npm ")
            || lower.starts_with("cargo ");

        if looks_subject {
            Some(line.clone())
        } else {
            None
        }
    })
}

fn normalized_line_for_matching(line: &str) -> String {
    line.trim_start_matches(|ch: char| !ch.is_alphanumeric() && ch != '/' && ch != '~' && ch != '.')
        .trim()
        .to_ascii_lowercase()
}

fn is_hint_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("esc to")
        || lower.contains("tab to")
        || lower.contains("ctrl+")
        || lower.contains("enter to")
}

fn relevant_raw_block(lines: &[String]) -> String {
    let start = lines.len().saturating_sub(24);
    lines[start..].join("\n")
}

fn looks_like_model_picker(lower: &str) -> bool {
    (lower.contains("model") && lower.contains("provider"))
        || lower.contains("select model")
        || lower.contains("choose model")
}

fn looks_like_question_prompt(lower: &str) -> bool {
    let has_question = lower.contains('?')
        || lower.contains("confirm")
        || lower.contains("continue")
        || lower.contains("choose")
        || lower.contains("select");
    let has_response_hint = lower.contains("yes")
        || lower.contains("no")
        || lower.contains("y/n")
        || lower.contains("enter to continue")
        || lower.contains("press enter")
        || lower.contains("select an option")
        || lower.contains("pick one");

    has_question && has_response_hint
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
    interaction_request: Option<InteractionRequest>,
) -> AdapterObservation {
    let interactive_kind = blocking_reason.map(str::to_string);
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
        need_interactive: interactive_kind.is_some(),
        interactive_kind,
        interaction_request,
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
        .trim_start_matches(['┃', '│', '┆', '┇', '┊', '┋', '¦', '|'])
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
    if trimmed.is_empty() {
        return None;
    }

    let mut candidate = None;

    for (open_paren, ch) in trimmed.char_indices() {
        if ch != '(' || is_within_quotes(trimmed, open_paren) {
            continue;
        }

        let Some(context_window) = previous_context_window_token(trimmed, open_paren) else {
            continue;
        };
        let rest = &trimmed[open_paren + 1..];
        let Some(close_paren) = rest.find(')') else {
            continue;
        };
        let percent_text = rest[..close_paren].trim();
        let Some(percent_text) = percent_text.strip_suffix('%') else {
            continue;
        };
        let Ok(context_usage_percent) = percent_text.trim().parse::<u8>() else {
            continue;
        };

        if context_usage_percent > 100 {
            continue;
        }

        candidate = Some((context_window, context_usage_percent));
    }

    candidate
}

fn previous_context_window_token(line: &str, open_paren: usize) -> Option<String> {
    let prefix = &line[..open_paren];
    let bytes = prefix.as_bytes();
    let mut end = prefix.len();

    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }

    if end == 0 {
        return None;
    }

    let mut start = end;
    while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }

    normalize_context_window_token(&prefix[start..end])
}

fn normalize_context_window_token(token: &str) -> Option<String> {
    let candidate = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | ',' | ';' | ':' | '[' | ']' | '(' | ')' | '{' | '}'
        )
    });

    if looks_like_context_window_token(candidate) {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn looks_like_context_window_token(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_digit() {
        return false;
    }

    let mut saw_digit = true;
    let mut saw_decimal = false;
    let mut saw_unit = false;

    for ch in chars {
        if ch.is_ascii_digit() {
            saw_digit = true;
            continue;
        }

        if ch == '.' && !saw_decimal && !saw_unit {
            saw_decimal = true;
            continue;
        }

        if matches!(ch, 'K' | 'M' | 'G' | 'k' | 'm' | 'g') && saw_digit && !saw_unit {
            saw_unit = true;
            continue;
        }

        return false;
    }

    saw_digit && saw_unit
}

fn is_within_quotes(line: &str, index: usize) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    for (offset, ch) in line.char_indices() {
        if offset >= index {
            break;
        }

        if escaped {
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        match ch {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            _ => {}
        }
    }

    in_single_quote || in_double_quote
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
        assert!(observation.need_interactive);
        assert_eq!(observation.interactive_kind.as_deref(), Some("permission"));
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
        let observation = OpencodeAdapter
            .observe("Prompt\n■■■⬝⬝⬝⬝⬝  esc interrupt  42.6K (21%)  ctrl+p commands\n".as_bytes());

        assert_eq!(observation.current_context_window.as_deref(), Some("42.6K"));
        assert_eq!(observation.current_context_usage_percent, Some(21));
    }

    #[test]
    fn ignores_quoted_context_usage_matches() {
        let observation = OpencodeAdapter.observe(
            concat!(
                "Prompt\n",
                "42.6K (21%)  ctrl+p commands\n",
                "[Pasted ~1 \"current_context_window\":\"■■■⬝⬝⬝⬝⬝  esc interrupt  59.2K (29%)\"\n",
            )
            .as_bytes(),
        );

        assert_eq!(observation.current_context_window.as_deref(), Some("42.6K"));
        assert_eq!(observation.current_context_usage_percent, Some(21));
    }

    #[test]
    fn detects_question_prompt_as_interactive() {
        let observation = OpencodeAdapter.observe(b"Continue? yes / no");

        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert_eq!(observation.ui_mode, UiMode::QuestionPrompt);
        assert_eq!(observation.blocking_reason.as_deref(), Some("question"));
        assert!(observation.need_interactive);
        assert_eq!(observation.interactive_kind.as_deref(), Some("question"));
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
    fn parses_structured_permission_options() {
        let screen = r#"
Permission request

Command: rm -rf ./safe-delete-target
Do you want to proceed?
❯ 1. Yes
  2. Yes, and always allow access to safe-delete-target/ from this project
  3. No

Esc to cancel · Tab to amend
"#;

        let observation = OpencodeAdapter.observe(screen.as_bytes());
        let interaction = observation
            .interaction_request
            .expect("interaction request");

        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert!(interaction.id.starts_with("opencode-permission-"));
        assert_eq!(interaction.kind, "permission");
        assert_eq!(interaction.source, "opencode");
        assert_eq!(interaction.confidence, 95);
        assert!(
            interaction
                .evidence
                .iter()
                .any(|item| item.label == "options")
        );
        assert_eq!(
            interaction.prompt.as_deref(),
            Some("Do you want to proceed?")
        );
        assert_eq!(interaction.options[0].action.as_deref(), Some("allow_once"));
        assert_eq!(interaction.title.as_deref(), Some("Permission request"));
        assert_eq!(
            interaction.subject.as_deref(),
            Some("Command: rm -rf ./safe-delete-target")
        );
        assert_eq!(interaction.options.len(), 3);
        assert_eq!(interaction.options[0].key, "1");
        assert_eq!(interaction.options[0].label, "Yes");
        assert!(interaction.options[0].selected);
        assert_eq!(
            interaction.options[1].action.as_deref(),
            Some("allow_persist")
        );
        assert_eq!(interaction.options[2].action.as_deref(), Some("deny"));
    }

    #[test]
    fn parses_external_directory_permission_buttons() {
        let screen = r#"
Permission required
← Access external directory ~/.config/opencode

Patterns

- /Users/doublemice/.config/opencode/*

Allow once   Allow always   Reject
"#;

        let observation = OpencodeAdapter.observe(screen.as_bytes());
        let interaction = observation
            .interaction_request
            .expect("interaction request");

        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert_eq!(observation.ui_mode, UiMode::PermissionPrompt);
        assert_eq!(observation.blocking_reason.as_deref(), Some("permission"));
        assert!(interaction.id.starts_with("opencode-external_directory-"));
        assert_eq!(interaction.kind, "external_directory");
        assert_eq!(interaction.confidence, 95);
        assert!(interaction.evidence.iter().any(|item| {
            item.label == "patterns" && item.value.contains("/Users/doublemice/.config/opencode/*")
        }));
        assert_eq!(
            interaction.subject.as_deref(),
            Some("← Access external directory ~/.config/opencode")
        );
        assert_eq!(interaction.options.len(), 3);
        assert_eq!(interaction.options[0].action.as_deref(), Some("allow_once"));
        assert_eq!(
            interaction.options[1].action.as_deref(),
            Some("allow_persist")
        );
        assert_eq!(interaction.options[2].action.as_deref(), Some("deny"));
        assert_eq!(
            OpencodeAdapter
                .submit_interaction(
                    &interaction,
                    &InteractionSubmission::Option {
                        key: "allow_once".to_string()
                    }
                )
                .unwrap(),
            b"\r"
        );
        assert_eq!(
            OpencodeAdapter
                .submit_interaction(
                    &interaction,
                    &InteractionSubmission::Option {
                        key: "allow_persist".to_string()
                    }
                )
                .unwrap(),
            b"\t\r"
        );
        assert_eq!(
            OpencodeAdapter
                .submit_interaction(
                    &interaction,
                    &InteractionSubmission::Option {
                        key: "deny".to_string()
                    }
                )
                .unwrap(),
            b"\x1b"
        );
    }

    #[test]
    fn synthesizes_allow_once_for_corrupted_button_row() {
        let screen = r#"
Permission required

┃  agents copy   Allow always   Reject
"#;

        let observation = OpencodeAdapter.observe(screen.as_bytes());
        let interaction = observation
            .interaction_request
            .expect("interaction request");

        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert_eq!(interaction.kind, "permission");
        assert_eq!(interaction.options.len(), 3);
        assert_eq!(interaction.options[0].label, "Allow once");
        assert_eq!(interaction.options[0].action.as_deref(), Some("allow_once"));
        assert_eq!(
            interaction.options[1].action.as_deref(),
            Some("allow_persist")
        );
        assert_eq!(interaction.options[2].action.as_deref(), Some("deny"));
    }

    #[test]
    fn ignores_non_permission_numbered_menus() {
        let screen = r#"
Select model
❯ 1. anthropic/claude
  2. openai/gpt
  3. local/qwen
"#;

        let observation = OpencodeAdapter.observe(screen.as_bytes());
        assert_ne!(observation.ui_mode, UiMode::PermissionPrompt);
        assert!(observation.interaction_request.is_none());
    }

    #[test]
    fn ignores_permission_configuration_summary() {
        let screen = r#"
让我读取主要的配置文件 opencode.json。

→ Read ~/.config/opencode/opencode.json

Thought: 用户要求查看 ~/ 下的 opencode 全局配置。我已经读取了主配置文件 ~/.config/opencode/opencode.json。

配置包含：
1. 插件：superpowers-lite
2. Provider: github-copilot，配置了多个模型：
   - claude-opus-4.7 (200k)
   - claude-sonnet-4.6 (200k)
   - gpt-5.4 (360k)
   - gpt-5.5 (360k)
   - gemini-3.1-pro-preview
3. Agents: 定义了多个 subagent:
   - implementer (gpt-5.4 high)
   - spec-reviewer (gpt-5.4)
4. 权限: 全局权限配置
5. snapshot: false

这是一个小任务，已经完成。我应该简洁地总结配置内容。

~/.config/opencode/opencode.json 配置概览：
全局权限: edit/write/bash/webfetch/websearch/skill/todowrite/todoread 均为 allow
"#;

        let observation = OpencodeAdapter.observe(screen.as_bytes());
        assert_eq!(observation.status, InstanceStatus::Ready);
        assert_eq!(observation.ui_mode, UiMode::Input);
        assert!(!observation.need_interactive);
        assert_ne!(observation.blocking_reason.as_deref(), Some("permission"));
        assert!(observation.interaction_request.is_none());
    }

    #[test]
    fn parses_question_tool_interaction() {
        let screen = r#"
Question

Which implementation path should I take?
❯ 1. Small focused patch
  2. Broader refactor

Type a custom answer or select an option
"#;

        let observation = OpencodeAdapter.observe(screen.as_bytes());
        let interaction = observation
            .interaction_request
            .expect("interaction request");

        assert_eq!(observation.status, InstanceStatus::Blocked);
        assert_eq!(observation.ui_mode, UiMode::QuestionPrompt);
        assert_eq!(observation.blocking_reason.as_deref(), Some("question"));
        assert_eq!(interaction.kind, "question");
        assert!(interaction.id.starts_with("opencode-question-"));
        assert_eq!(interaction.confidence, 90);
        assert!(
            interaction
                .evidence
                .iter()
                .any(|item| { item.label == "custom_answer" && item.value == "allowed" })
        );
        assert_eq!(interaction.title.as_deref(), Some("Question"));
        assert_eq!(
            interaction.prompt.as_deref(),
            Some("Which implementation path should I take?")
        );
        assert!(interaction.custom_answer_allowed);
        assert_eq!(interaction.options.len(), 2);
        assert_eq!(interaction.options[0].key, "1");
        assert!(interaction.options[0].selected);
        assert_eq!(interaction.options[0].action, None);
        assert_eq!(
            OpencodeAdapter
                .submit_interaction(
                    &interaction,
                    &InteractionSubmission::Option {
                        key: "2".to_string()
                    }
                )
                .unwrap(),
            b"2\r"
        );
        assert_eq!(
            OpencodeAdapter
                .submit_interaction(
                    &interaction,
                    &InteractionSubmission::CustomAnswer {
                        answer: "Use the smaller patch".to_string()
                    }
                )
                .unwrap(),
            b"Use the smaller patch\r"
        );
    }

    #[test]
    fn switch_model_is_explicitly_unsupported_for_now() {
        let error = OpencodeAdapter
            .switch_model(b"anthropic/claude")
            .unwrap_err();
        assert_eq!(error.code(), "unsupported_action");
    }

    #[test]
    fn next_model_uses_f2() {
        assert_eq!(OpencodeAdapter.next_model().unwrap(), b"\x1bOQ");
    }

    #[test]
    fn previous_model_uses_shift_f2() {
        assert_eq!(OpencodeAdapter.previous_model().unwrap(), b"\x1b[1;2Q");
    }
}
