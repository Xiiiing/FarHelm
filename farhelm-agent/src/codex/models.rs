//! A bounded projection of the configured native provider's model picker.
use super::*;
use farhelm_protocol::{CodexModelList, CodexModelOption, valid_model_id, valid_reasoning_effort};

pub(super) async fn list(connection: &Connection) -> Result<CodexModelList> {
    let mut cursor = Value::Null;
    let mut seen = std::collections::HashSet::new();
    let mut models = BTreeMap::new();
    for _ in 0..16 {
        let page = connection
            .request(
                "model/list",
                json!({"limit":100,"cursor":cursor,"includeHidden":false}),
            )
            .await?;
        for row in page["data"].as_array().context("codex_models_invalid")? {
            if row["hidden"] == true {
                continue;
            }
            let Some(model) = row["model"].as_str().filter(|m| valid_model_id(m)) else {
                continue;
            };
            let efforts: Vec<String> = row["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    r["reasoningEffort"]
                        .as_str()
                        .filter(|e| valid_reasoning_effort(e))
                        .map(str::to_owned)
                })
                .collect();
            let Some(default) = row["defaultReasoningEffort"]
                .as_str()
                .filter(|e| efforts.iter().any(|v| v == e))
            else {
                continue;
            };
            let display = row["displayName"]
                .as_str()
                .filter(|s| {
                    !s.is_empty()
                        && s.chars().count() <= 128
                        && !s.chars().any(char::is_control)
                        && !s.contains(['/', '\\'])
                })
                .unwrap_or(model);
            models.insert(
                model.to_owned(),
                CodexModelOption {
                    model: model.into(),
                    display_name: display.into(),
                    reasoning_efforts: efforts,
                    default_reasoning_effort: default.into(),
                    is_default: row["isDefault"] == true,
                },
            );
            ensure!(models.len() <= 512, "codex_models_capacity");
        }
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(CodexModelList {
                models: models.into_values().collect(),
            });
        }
        ensure!(
            cursor.as_str().is_some_and(|s| s.len() <= 512) && seen.insert(cursor.to_string()),
            "codex_models_cursor_invalid"
        );
    }
    bail!("codex_models_capacity")
}
