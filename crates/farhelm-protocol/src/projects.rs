//! Path-free project management messages. Directory identifiers are Agent-issued capabilities.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDirectory {
    pub directory_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRoots {
    pub roots: Vec<ProjectDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryPage {
    pub directory: ProjectDirectory,
    pub parent_id: Option<String>,
    pub entries: Vec<ProjectDirectory>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AddProjectRequest {
    Attach {
        agent_id: String,
        directory_id: String,
    },
    Create {
        agent_id: String,
        directory_id: String,
        name: String,
    },
    AttachTo {
        agent_id: String,
        project_id: String,
        directory_id: String,
    },
}

impl AddProjectRequest {
    pub fn agent_id(&self) -> &str {
        match self {
            Self::Attach { agent_id, .. }
            | Self::Create { agent_id, .. }
            | Self::AttachTo { agent_id, .. } => agent_id,
        }
    }
    pub fn directory_id(&self) -> &str {
        match self {
            Self::Attach { directory_id, .. }
            | Self::Create { directory_id, .. }
            | Self::AttachTo { directory_id, .. } => directory_id,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPreference {
    pub agent_id: String,
    pub project_id: String,
    pub display_name: Option<String>,
    pub hidden: bool,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_order: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPreferences {
    pub revision: u64,
    pub projects: Vec<ProjectPreference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectPreferences {
    pub revision: u64,
    pub projects: Vec<ProjectPreference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectInfo {
    pub can_create_worktree: bool,
    #[serde(default)]
    pub directories: Vec<ProjectMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMember {
    pub directory_id: String,
    pub name: String,
    pub is_primary: bool,
    #[serde(default)]
    pub can_create_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivePreview {
    pub session_ids: Vec<String>,
    pub fingerprint: String,
    pub can_archive: bool,
    pub reason: Option<String>,
}

pub fn valid_directory_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.trim() == name
        && !matches!(name, "." | "..")
        && !name.starts_with(".farhelm-")
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
}

pub fn valid_directory_id(id: &str) -> bool {
    id.len() == 36
        && (id.starts_with("dir_") || id.starts_with("rtd_"))
        && id[4..].bytes().all(|c| c.is_ascii_hexdigit())
}

pub fn valid_member_id(id: &str) -> bool {
    id.len() == 36 && id.starts_with("mem_") && id[4..].bytes().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_management_wire_fixture_roundtrips_without_paths() {
        fn roundtrip<T: serde::de::DeserializeOwned + Serialize>(value: &serde_json::Value) {
            assert_eq!(
                serde_json::to_value(serde_json::from_value::<T>(value.clone()).unwrap()).unwrap(),
                *value
            );
        }
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/project-management.json"))
                .unwrap();
        roundtrip::<ProjectRoots>(&fixture["roots"]);
        roundtrip::<DirectoryPage>(&fixture["directory"]);
        roundtrip::<AddProjectRequest>(&fixture["create"]);
        roundtrip::<ProjectPreferences>(&fixture["preferences"]);
        roundtrip::<ArchivePreview>(&fixture["preview"]);
        roundtrip::<crate::AgentCommand>(&fixture["command"]);
    }
    #[test]
    fn requests_are_path_free_and_names_are_single_components() {
        let wire = r#"{"kind":"create","agent_id":"gpu-a","directory_id":"rtd_0123456789abcdef0123456789abcdef","name":"新项目"}"#;
        let request: AddProjectRequest = serde_json::from_str(wire).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::from_str::<serde_json::Value>(wire).unwrap()
        );
        assert!(
            serde_json::from_str::<AddProjectRequest>(
                r#"{"kind":"attach","agent_id":"a","directory_id":"x","path":"/private"}"#
            )
            .is_err()
        );
        for name in [
            "",
            "..",
            "a/b",
            "a\\b",
            "\n",
            ".farhelm-staged",
            " trailing ",
        ] {
            assert!(!valid_directory_name(name));
        }
        assert!(valid_directory_name("研究 project"));
    }
}
