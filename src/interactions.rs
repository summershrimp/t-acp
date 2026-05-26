use crate::api::{InteractionEvidence, InteractionRequest, SubmitInteractionRequest};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteractionSubmission {
    Option { key: String },
    CustomAnswer { answer: String },
}

impl InteractionSubmission {
    pub fn label(&self) -> &str {
        match self {
            Self::Option { key } => key,
            Self::CustomAnswer { .. } => "custom_answer",
        }
    }

    pub fn option_key(&self) -> Option<&str> {
        match self {
            Self::Option { key } => Some(key),
            Self::CustomAnswer { .. } => None,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InteractionSubmissionError {
    #[error("provide exactly one of option_key or custom_answer")]
    Ambiguous,
}

pub fn validate_submission(
    request: &SubmitInteractionRequest,
) -> Result<InteractionSubmission, InteractionSubmissionError> {
    let option_key = request
        .option_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let custom_answer = request
        .custom_answer
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (option_key, custom_answer) {
        (Some(key), None) => Ok(InteractionSubmission::Option {
            key: key.to_string(),
        }),
        (None, Some(answer)) => Ok(InteractionSubmission::CustomAnswer {
            answer: answer.to_string(),
        }),
        _ => Err(InteractionSubmissionError::Ambiguous),
    }
}

pub fn with_stable_id(mut request: InteractionRequest) -> InteractionRequest {
    request.id = stable_id(&request);
    request
}

pub fn stable_id(request: &InteractionRequest) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    update_hash(&mut hash, &request.source);
    update_hash(&mut hash, &request.kind);
    update_hash(&mut hash, request.title.as_deref().unwrap_or(""));
    update_hash(&mut hash, request.subject.as_deref().unwrap_or(""));
    update_hash(&mut hash, request.prompt.as_deref().unwrap_or(""));
    for option in &request.options {
        update_hash(&mut hash, &option.key);
        update_hash(&mut hash, &option.label);
        update_hash(&mut hash, option.action.as_deref().unwrap_or(""));
    }
    format!("{}-{}-{hash:016x}", request.source, request.kind)
}

pub fn push_evidence(evidence: &mut Vec<InteractionEvidence>, label: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    evidence.push(InteractionEvidence {
        label: label.to_string(),
        value: value.to_string(),
    });
}

fn update_hash(hash: &mut u64, value: &str) {
    for byte in value.as_bytes() {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x100000001b3);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(option_key: Option<&str>, custom_answer: Option<&str>) -> SubmitInteractionRequest {
        SubmitInteractionRequest {
            interaction_id: "interaction-1".to_string(),
            option_key: option_key.map(str::to_string),
            custom_answer: custom_answer.map(str::to_string),
        }
    }

    #[test]
    fn validate_submission_accepts_one_option_key() {
        assert_eq!(
            validate_submission(&request(Some(" allow_once "), None)).unwrap(),
            InteractionSubmission::Option {
                key: "allow_once".to_string()
            }
        );
    }

    #[test]
    fn validate_submission_accepts_one_custom_answer() {
        assert_eq!(
            validate_submission(&request(None, Some(" use plan A "))).unwrap(),
            InteractionSubmission::CustomAnswer {
                answer: "use plan A".to_string()
            }
        );
    }

    #[test]
    fn validate_submission_rejects_empty_or_ambiguous_payloads() {
        assert_eq!(
            validate_submission(&request(None, None)).unwrap_err(),
            InteractionSubmissionError::Ambiguous
        );
        assert_eq!(
            validate_submission(&request(Some("1"), Some("answer"))).unwrap_err(),
            InteractionSubmissionError::Ambiguous
        );
    }

    #[test]
    fn stable_id_ignores_raw_screen_redraw_noise() {
        let mut first = InteractionRequest {
            id: String::new(),
            kind: "external_directory".to_string(),
            source: "opencode".to_string(),
            title: Some("Permission required".to_string()),
            subject: Some("Access external directory ~/.config/opencode".to_string()),
            prompt: None,
            options: vec![crate::api::InteractionOption {
                key: "allow_once".to_string(),
                label: "Allow once".to_string(),
                selected: true,
                action: Some("allow_once".to_string()),
            }],
            custom_answer_allowed: false,
            confidence: 95,
            evidence: Vec::new(),
            raw: "frame 1".to_string(),
        };
        let mut second = first.clone();
        second.raw = "frame 2 with spinner redraw".to_string();

        first = with_stable_id(first);
        second = with_stable_id(second);

        assert_eq!(first.id, second.id);
    }
}
