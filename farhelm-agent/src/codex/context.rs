//! Allowlisted native configuration for the authenticated, ephemeral read relay.
use farhelm_protocol::CodexSessionContext;
use serde_json::Value;

pub fn project(value: &Value, previous: Option<&CodexSessionContext>) -> CodexSessionContext {
    let mut context = previous.cloned().unwrap_or_default();
    if let Some(ephemeral) = value["thread"]["ephemeral"]
        .as_bool()
        .or_else(|| value["ephemeral"].as_bool())
    {
        context.ephemeral = Some(ephemeral);
    }
    if let Some(model) = value.get("model") {
        context.model = model
            .as_str()
            .filter(|name| farhelm_protocol::valid_model_id(name))
            .map(str::to_owned);
    }
    if let Some(effort) = value.get("reasoningEffort") {
        context.reasoning_effort = choice(
            effort,
            &[
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ],
        );
    }
    if let Some(sandbox) = value.get("sandbox").or_else(|| value.get("sandboxPolicy")) {
        context.sandbox = Some(
            match sandbox["type"].as_str().or_else(|| sandbox.as_str()) {
                Some("readOnly" | "read-only") => "read-only",
                Some("workspaceWrite" | "workspace-write") => "workspace-write",
                Some("dangerFullAccess" | "danger-full-access") => "danger-full-access",
                Some("externalSandbox" | "external-sandbox") => "external-sandbox",
                _ => "custom",
            }
            .to_owned(),
        );
        if sandbox.is_null() {
            context.sandbox = None;
        }
    }
    if let Some(approval) = value.get("approvalPolicy") {
        context.approval_policy = Some(
            choice(
                approval,
                &["never", "on-request", "on-failure", "untrusted"],
            )
            .unwrap_or_else(|| "custom".into()),
        );
    }
    if let Some(reviewer) = value.get("approvalsReviewer") {
        context.approvals_reviewer =
            choice(reviewer, &["user", "auto_review", "guardian_subagent"]);
    }
    context
}

fn choice(value: &Value, options: &[&str]) -> Option<String> {
    value
        .as_str()
        .filter(|v| options.contains(v))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn native_context_projects_only_bounded_settings_not_paths_or_credentials() {
        let input = json!({"model":"gpt-5.4","reasoningEffort":"high","sandbox":{"type":"workspaceWrite","writableRoots":["/secret/repo"]},"approvalPolicy":"on-request","approvalsReviewer":"user","cwd":"/private/project","api_key":"credential","instructions":"private body"});
        let context = project(&input, None);
        assert_eq!(context.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(context.sandbox.as_deref(), Some("workspace-write"));
        let encoded = serde_json::to_string(&context).unwrap();
        for private in [
            "/secret",
            "/private",
            "credential",
            "private body",
            "writableRoots",
        ] {
            assert!(!encoded.contains(private));
        }
        let updated = project(
            &json!({"model":"/private/model","reasoningEffort":"unknown"}),
            Some(&context),
        );
        assert!(updated.model.is_none());
        assert!(updated.reasoning_effort.is_none());
        assert_eq!(updated.sandbox, context.sandbox);
    }
}
