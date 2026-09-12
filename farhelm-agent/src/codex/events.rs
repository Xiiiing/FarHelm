//! Connection-wide display projection. Only explicit display fields leave this module.
use super::*;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct DisplayEvent {
    pub kind: &'static str,
    pub session: String,
    pub data: Value,
}

#[derive(Default)]
pub(super) struct Projector {
    text: HashMap<(String, String, String), history::StreamText>,
}
impl Projector {
    pub fn accept(&mut self, event: &Value, output: &broadcast::Sender<DisplayEvent>) {
        let method = event["method"].as_str().unwrap_or_default();
        let p = &event["params"];
        let Some(session) = p["threadId"]
            .as_str()
            .or_else(|| p["thread"]["id"].as_str())
        else {
            return;
        };
        let turn = p["turnId"]
            .as_str()
            .or_else(|| p["turn"]["id"].as_str())
            .unwrap_or_default();
        if method == "turn/started" {
            let _ = output.send(DisplayEvent {
                kind: "codex.turn.started",
                session: session.into(),
                data: json!({"session_id":session,"turn_id":turn,"status":"running"}),
            });
        }
        if method == "item/agentMessage/delta" {
            if let (Some(item), Some(delta)) = (p["itemId"].as_str(), p["delta"].as_str()) {
                let key = (session.into(), turn.into(), item.into());
                if self.text.len() >= 4096 && !self.text.contains_key(&key) {
                    self.changed(session, output);
                    return;
                }
                let (offset, text) = self.text.entry(key).or_default().feed(delta, false);
                Self::delta(session, turn, item, offset, text, output);
            }
        } else if method == "turn/completed" {
            self.text.retain(|(s, t, item), buffer| {
                if s == session && t == turn {
                    let (offset, text) = buffer.feed("", true);
                    Self::delta(session, turn, item, offset, text, output);
                    false
                } else {
                    true
                }
            });
            let status = p["turn"]["status"].as_str().unwrap_or("unknown");
            let _ = output.send(DisplayEvent {
                kind: if status == "completed" {
                    "codex.turn.completed"
                } else {
                    "codex.turn.failed"
                },
                session: session.into(),
                data: json!({"session_id":session,"turn_id":turn,"status":status}),
            });
            self.changed(session, output);
        } else if matches!(
            method,
            "turn/started"
                | "item/started"
                | "item/completed"
                | "turn/plan/updated"
                | "turn/diff/updated"
                | "serverRequest/resolved"
                | "farhelm/serverRequest"
                | "farhelm/serverRequestResolved"
                | "model/rerouted"
                | "warning"
        ) || method.starts_with("thread/")
        {
            self.changed(session, output);
        }
    }
    fn changed(&self, session: &str, output: &broadcast::Sender<DisplayEvent>) {
        let _ = output.send(DisplayEvent {
            kind: "codex.native.changed",
            session: session.into(),
            data: json!({}),
        });
    }
    fn delta(
        session: &str,
        turn: &str,
        item: &str,
        offset: usize,
        text: String,
        output: &broadcast::Sender<DisplayEvent>,
    ) {
        if !text.is_empty() {
            let _ = output.send(DisplayEvent {kind:"codex.message.delta", session:session.into(),data:json!({"session_id":session,"turn_id":turn,"item_id":item,"text_offset":offset,"delta":text})});
        }
    }
}

pub(super) async fn observe_activity(inner: &Inner, event: &Value) {
    let params = &event["params"];
    let Some(session) = params["threadId"].as_str() else {
        return;
    };
    let (key, mut value) = match event["method"].as_str() {
        Some("turn/plan/updated") => ("plan", params["plan"].clone()),
        Some("thread/tokenUsage/updated") => ("usage", params["tokenUsage"].clone()),
        Some("thread/goal/updated") => ("goal", params["goal"].clone()),
        Some("model/rerouted") => ("model", params["toModel"].clone()),
        Some("warning") => ("warning", params["message"].clone()),
        _ => return,
    };
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        return;
    }
    fn redact(value: &mut Value) {
        match value {
            Value::String(text) => *text = history::redact_paths(text),
            Value::Array(items) => items.iter_mut().for_each(redact),
            Value::Object(fields) => fields.values_mut().for_each(redact),
            _ => {}
        }
    }
    redact(&mut value);
    let mut snapshots = inner.live_activity.write().await;
    if snapshots.len() >= 64 && !snapshots.contains_key(session) {
        snapshots.clear();
    }
    snapshots.entry(session.into()).or_insert_with(|| json!({}))[key] = value;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn externally_started_turns_and_interactions_use_shared_projection() {
        let (tx, mut rx) = broadcast::channel(32);
        let mut projector = Projector::default();
        for event in [
            json!({"method":"item/agentMessage/delta","params":{"threadId":"external","turnId":"q1","itemId":"i","delta":"hello /secret/"}}),
            json!({"method":"item/agentMessage/delta","params":{"threadId":"external","turnId":"q1","itemId":"i","delta":"file done"}}),
            json!({"method":"turn/completed","params":{"threadId":"external","turn":{"id":"q1","status":"completed"}}}),
            json!({"method":"farhelm/serverRequest","params":{"threadId":"external","command":"PRIVATE_COMMAND"}}),
        ] {
            projector.accept(&event, &tx);
        }
        let mut text = String::new();
        let mut changes = 0;
        while let Ok(event) = rx.try_recv() {
            assert_eq!(event.session, "external");
            assert!(!event.data.to_string().contains("PRIVATE_COMMAND"));
            if event.kind == "codex.message.delta" {
                assert_eq!(event.data["text_offset"], text.chars().count());
                text.push_str(event.data["delta"].as_str().unwrap());
            } else if event.kind == "codex.native.changed" {
                changes += 1;
            }
        }
        assert_eq!(text, "hello [local path] done");
        assert_eq!(changes, 2);
        assert!(projector.text.is_empty());
    }
}
