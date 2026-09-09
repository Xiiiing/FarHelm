//! Explicit, path-free native Codex operations exposed by FarHelm.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum NativeOperation {
    QueueAdd {
        input: Vec<NativeInput>,
        client_message_id: String,
    },
    QueueUpdate {
        submission_id: String,
        input: Vec<NativeInput>,
        revision: String,
    },
    QueueDelete {
        submission_id: String,
        revision: String,
    },
    QueueReorder {
        submission_ids: Vec<String>,
        revision: String,
    },
    QueueStart {
        submission_id: Option<String>,
        revision: String,
    },
    SectionCreate {
        name: String,
    },
    SectionRename {
        section_id: String,
        name: String,
    },
    SectionDelete {
        section_id: String,
    },
    SectionMove {
        section_id: Option<String>,
        before_thread_id: Option<String>,
    },
    Fork {
        last_turn_id: String,
        ephemeral: bool,
    },
    Revert {
        before_turn_id: String,
    },
    Delete {
        impact_fingerprint: String,
    },
    Compact,
    GoalSet {
        objective: Option<String>,
        status: Option<GoalStatus>,
        token_budget: Option<u64>,
    },
    GoalClear,
    ThreadSettings {
        settings: NativeSettings,
    },
    TurnSettings {
        turn_id: String,
        settings: NativeSettings,
    },
    Review {
        target: ReviewTarget,
    },
    InteractionAnswer {
        request_token: String,
        answer: Value,
    },
    AttachmentBegin {
        attachment_id: String,
        mime_type: String,
        size_bytes: u64,
        ephemeral: bool,
    },
    AttachmentChunk {
        attachment_id: String,
        offset: u64,
        data_base64: String,
    },
    AttachmentFinish {
        attachment_id: String,
    },
}

impl NativeOperation {
    #[must_use]
    pub const fn contains_private_body(&self) -> bool {
        matches!(
            self,
            Self::QueueAdd { .. }
                | Self::QueueUpdate { .. }
                | Self::GoalSet { .. }
                | Self::InteractionAnswer { .. }
                | Self::AttachmentChunk { .. }
        )
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::QueueAdd {
                input,
                client_message_id,
            } => valid_inputs(input) && valid_id(client_message_id),
            Self::QueueUpdate {
                submission_id,
                input,
                revision,
            } => valid_id(submission_id) && valid_inputs(input) && valid_revision(revision),
            Self::QueueDelete {
                submission_id,
                revision,
            } => valid_id(submission_id) && valid_revision(revision),
            Self::QueueReorder {
                submission_ids,
                revision,
            } => {
                !submission_ids.is_empty()
                    && submission_ids.len() <= 256
                    && submission_ids.iter().all(|id| valid_id(id))
                    && valid_revision(revision)
            }
            Self::QueueStart {
                submission_id,
                revision,
            } => submission_id.as_ref().is_none_or(|id| valid_id(id)) && valid_revision(revision),
            Self::SectionCreate { name } => valid_name(name),
            Self::SectionDelete { section_id } => valid_id(section_id),
            Self::SectionMove {
                section_id,
                before_thread_id,
            } => {
                section_id.as_ref().is_none_or(|id| valid_id(id))
                    && before_thread_id.as_ref().is_none_or(|id| valid_id(id))
            }
            Self::Fork { last_turn_id, .. }
            | Self::Revert {
                before_turn_id: last_turn_id,
            } => valid_id(last_turn_id),
            Self::Delete { impact_fingerprint } => valid_revision(impact_fingerprint),
            Self::Compact | Self::GoalClear => true,
            Self::GoalSet {
                objective,
                token_budget,
                ..
            } => {
                objective
                    .as_ref()
                    .is_none_or(|v| !v.trim().is_empty() && v.len() <= 16_384)
                    && token_budget.is_none_or(|v| v > 0)
            }
            Self::ThreadSettings { settings } => settings.is_valid(),
            Self::TurnSettings { turn_id, settings } => valid_id(turn_id) && settings.is_valid(),
            Self::Review { target } => target.is_valid(),
            Self::InteractionAnswer {
                request_token,
                answer,
            } => {
                valid_id(request_token)
                    && serde_json::to_vec(answer).is_ok_and(|v| v.len() <= 64 * 1024)
            }
            Self::AttachmentBegin {
                attachment_id,
                mime_type,
                size_bytes,
                ..
            } => {
                valid_id(attachment_id)
                    && matches!(
                        mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp"
                    )
                    && (1..=20 * 1024 * 1024).contains(size_bytes)
            }
            Self::AttachmentChunk {
                attachment_id,
                offset,
                data_base64,
            } => {
                valid_id(attachment_id)
                    && *offset <= 20 * 1024 * 1024
                    && !data_base64.is_empty()
                    && data_base64.len() <= 360 * 1024
            }
            Self::AttachmentFinish { attachment_id } => valid_id(attachment_id),
            Self::SectionRename { section_id, name } => valid_id(section_id) && valid_name(name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeInput {
    Text { text: String },
    Skill { skill_id: String },
    Image { attachment_id: String },
}

fn valid_inputs(input: &[NativeInput]) -> bool {
    !input.is_empty()
        && input.len() <= 9
        && input.iter().all(|item| match item {
            NativeInput::Text { text } => !text.trim().is_empty() && text.len() <= 1024 * 1024,
            NativeInput::Skill { skill_id }
            | NativeInput::Image {
                attachment_id: skill_id,
            } => valid_id(skill_id),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSettings {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub approval_policy: Option<String>,
    pub approvals_reviewer: Option<String>,
    pub collaboration_mode: Option<String>,
    pub permissions: Option<String>,
    pub service_tier: Option<String>,
}

impl NativeSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.collaboration_mode
            .as_ref()
            .is_none_or(|_| self.model.is_some())
            && [
                &self.model,
                &self.reasoning_effort,
                &self.approval_policy,
                &self.approvals_reviewer,
                &self.collaboration_mode,
                &self.permissions,
                &self.service_tier,
            ]
            .into_iter()
            .flatten()
            .all(|v| {
                !v.is_empty()
                    && v.len() <= 128
                    && v.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/')
                    })
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReviewTarget {
    UncommittedChanges,
    BaseBranch { branch: String },
    Commit { sha: String },
}

impl ReviewTarget {
    fn is_valid(&self) -> bool {
        match self {
            Self::UncommittedChanges => true,
            Self::BaseBranch { branch } => {
                !branch.is_empty()
                    && branch.len() <= 256
                    && !branch.chars().any(char::is_whitespace)
            }
            Self::Commit { sha } => {
                (7..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
            }
        }
    }
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 128
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}
fn valid_revision(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOperationRequest {
    pub idempotency_key: String,
    #[serde(flatten)]
    pub operation: NativeOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeReadKind {
    Queue,
    Sections,
    Goal,
    Skills,
    Settings,
    Permissions,
    DeleteImpact,
    PendingInteractions,
    ImageResource,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn body_classification_and_image_limits_are_explicit() {
        let metadata = NativeOperation::Compact;
        assert!(!metadata.contains_private_body());
        let body = NativeOperation::AttachmentChunk {
            attachment_id: "att_a".into(),
            offset: 0,
            data_base64: "AA==".into(),
        };
        assert!(body.contains_private_body());
        assert!(body.is_valid());
        assert!(
            !NativeOperation::AttachmentBegin {
                attachment_id: "att_a".into(),
                mime_type: "image/gif".into(),
                size_bytes: 1,
                ephemeral: false
            }
            .is_valid()
        );
        assert!(
            !NativeSettings {
                collaboration_mode: Some("plan".into()),
                ..Default::default()
            }
            .is_valid()
        );
    }
}
