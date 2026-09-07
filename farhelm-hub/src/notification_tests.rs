use super::*;

fn event(id: &str, sequence: u64, operation: &str, kind: &str) -> AgentEvent {
    AgentEvent {
        protocol: FARHELM_PROTOCOL.into(),
        agent_id: "agent".into(),
        event_id: id.into(),
        sequence,
        event_type: kind.into(),
        created_at_unix: 100,
        payload: json!({"operation_id":operation,"session_id":"s","data":{"turn_id":"t","status":"completed","turn":{"items":[{"text":"PRIVATE_BODY"}]},"cwd":"/private/project"}}),
    }
}

#[test]
fn duplicate_terminal_paths_project_one_notification_per_execution() {
    let store = EventStore::open(Path::new(":memory:")).unwrap();
    store
        .ingest(
            "agent",
            &[
                event("worker", 1, "job", "codex.turn.completed"),
                event("result", 2, "job", "codex.turn.completed"),
                event("restart", 3, "job", "codex.turn.orphaned"),
            ],
        )
        .unwrap();
    let list = store.notifications(&ListQuery::default()).unwrap();
    assert_eq!(list["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(list["notifications"][0]["state"], "succeeded");
    assert!(
        !store
            .lock()
            .unwrap()
            .query_row(
                "SELECT group_concat(payload_json) FROM agent_events",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap()
            .contains("PRIVATE_BODY")
    );
    store
        .notification_write("read-key", "notification.read", "1", &json!({}), 101)
        .unwrap();
    assert_eq!(
        store.notifications(&ListQuery::default()).unwrap()["unread_count"],
        0
    );
    assert!(
        store
            .notification_write("read-key", "notification.read", "2", &json!({}), 101)
            .is_err()
    );
}

#[test]
fn multiple_devices_retry_after_expiry_and_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hub.db");
    let store = EventStore::open(&path).unwrap();
    for suffix in ["one", "two"] {
        store
            .save_push_subscription(
                &format!("https://push.test/{suffix}"),
                &"a".repeat(88),
                &"b".repeat(22),
                100,
            )
            .unwrap();
    }
    store
        .ingest(
            "agent",
            &[event("terminal", 1, "job", "codex.turn.completed")],
        )
        .unwrap();
    let mut pending = store.pending_notifications(100).unwrap();
    assert_eq!(pending.len(), 2);
    let first = pending.remove(0);
    let second = pending.remove(0);
    store
        .finish_notification_delivery(&first, Some((false, "HTTP 429", Some(7200))), 100)
        .unwrap();
    store
        .finish_notification_delivery(&second, None, 100)
        .unwrap();
    assert!(store.pending_notifications(7299).unwrap().is_empty());
    drop(store);
    let store = EventStore::open(&path).unwrap();
    assert_eq!(store.pending_notifications(7300).unwrap().len(), 1);
    assert!(store.pending_notifications(86500).unwrap().is_empty());
    let details = store.notification_deliveries(1).unwrap();
    assert!(
        details
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["state"] == "accepted")
    );
    assert!(
        details
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["state"] == "expired")
    );
    let devices = store.devices().unwrap();
    let id = devices["devices"][0]["id"].as_str().unwrap();
    assert!(!devices.to_string().contains("https://"));
    assert!(!devices.to_string().contains("p256dh"));
    store
        .notification_write("test-key", "notification.test", id, &json!({}), 86500)
        .unwrap();
    store
        .notification_write("test-key", "notification.test", id, &json!({}), 86501)
        .unwrap();
    assert_eq!(store.pending_notifications(86501).unwrap().len(), 1);
    let delivery = store.pending_notifications(86501).unwrap().remove(0);
    store
        .finish_notification_delivery(&delivery, Some((true, "HTTP 410", None)), 86501)
        .unwrap();
    assert_eq!(
        store.devices().unwrap()["devices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn script_results_have_no_synthetic_pid_and_are_filterable() {
    let store = EventStore::open(Path::new(":memory:")).unwrap();
    let mut report = event("report", 1, "", "experiment.reported");
    report.payload = json!({"report_id":"r","run_id":"batch","project_id":"p","name":"8 rounds","state":"failed","source":"script_report","message":"failure details","updated_at_unix":100});
    store.ingest("agent", &[report]).unwrap();
    let page = store
        .experiment_runs(&ListQuery {
            id: Some("r".into()),
            source: Some("script_report".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page["experiments"].as_array().unwrap().len(), 1);
    assert!(page["experiments"][0].get("pid").is_none());
    assert_eq!(page["experiments"][0]["message"], "failure details");
}
