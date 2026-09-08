use super::*;

#[tokio::test]
async fn archive_checks_subtree_then_preserves_ids_and_shared_gates() {
    use crate::experiment_store::ExperimentStore;
    use farhelm_protocol::{AgentCommand, CommandAction, FARHELM_PROTOCOL};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let unauthorized = temp.path().join("unauthorized");
    std::fs::create_dir(&unauthorized).unwrap();
    let saved = temp.path().join("saved.jsonl");
    std::fs::write(&saved, b"synthetic history").unwrap();
    let bin = temp.path().join("native");
    let control = temp.path().join("control");
    std::fs::write(&control, b"idle").unwrap();
    let script = r#"#!/usr/bin/python3
import sys,json,pathlib
if '--version' in sys.argv: print('codex-cli 0.153.4');sys.exit()
statefile=pathlib.Path(CONTROL+'.json')
archived=set(json.loads(statefile.read_text())) if statefile.exists() else set()
def row(id):
 mode=pathlib.Path(CONTROL).read_text()
 return dict(id=id,cwd=UNAUTHORIZED if id=='child' and mode=='unauthorized' else ROOT,name=None,parentThreadId=None if id=='root' else 'root',updatedAt=2 if mode=='changed' else 1,ephemeral=mode=='unsaved',path=SAVED,status={'type':'notLoaded' if id in archived else 'active' if id=='child' and mode=='active' else 'idle'})
for line in sys.stdin:
 q=json.loads(line)
 if 'id' not in q:continue
 m=q.get('method');p=q.get('params',{});result={}
 if m=='initialize':result={'userAgent':'fixture'}
 elif m=='account/read':result={'requiresOpenaiAuth':False,'account':None}
 elif m=='thread/read':result={'thread':row(p['threadId'])}
 elif m=='thread/list':
  if 'subAgentThreadSpawn' not in p.get('sourceKinds',[]):
   print(json.dumps({'id':q['id'],'error':{'code':-32600,'message':'incomplete source range'}}),flush=True);continue
  ids=['child'] if p.get('ancestorThreadId') else ['root','child']
  result={'data':[row(id) for id in ids if (id in archived)==p.get('archived',False)],'nextCursor':None}
 elif m=='thread/queue/list' and p['threadId'] in archived:
  print(json.dumps({'id':q['id'],'error':{'code':-32600,'message':'archived'}}),flush=True);continue
 elif m in ['thread/backgroundTerminals/list','thread/queue/list']:
  busy=(m=='thread/queue/list' and pathlib.Path(CONTROL).read_text()=='queued') or (m=='thread/backgroundTerminals/list' and pathlib.Path(CONTROL).read_text()=='background')
  result={'data':[{'id':'pending'}] if busy and p['threadId']=='child' else [],'nextCursor':None}
 elif m=='thread/archive':
  archived.update(['root','child']);statefile.write_text(json.dumps(list(archived)))
  if pathlib.Path(CONTROL).read_text()=='disconnect':sys.exit(1)
 elif m=='thread/unarchive':archived.discard(p['threadId']);statefile.write_text(json.dumps(list(archived)))
 print(json.dumps({'id':q['id'],'result':result}),flush=True)
"#;
    transport::write_fixture_executable(
        &bin,
        &script
            .replace("ROOT", &json!(root).to_string())
            .replace("UNAUTHORIZED", &json!(unauthorized).to_string())
            .replace("SAVED", &json!(saved).to_string())
            .replace("CONTROL", &json!(control).to_string()),
    );
    let store = ExperimentStore::open(&temp.path().join("agent.db")).unwrap();
    let (candidate, _) = store
        .upsert_discovered_project(&root, "Project", "p", 2, 1)
        .unwrap();
    store
        .approve_candidates(&[candidate.candidate_id], 2)
        .unwrap();
    store
        .bind_session("root", "p", &root, "inspect", 2)
        .unwrap();
    let native = Codex::new(Some(bin.clone()));
    assert_eq!(native.index(true).await.unwrap().len(), 2);
    for (mode, expected) in [
        ("active", "busy"),
        ("queued", "busy"),
        ("background", "busy"),
        ("unauthorized", "unapproved"),
        ("unsaved", "unsaved"),
    ] {
        std::fs::write(&control, mode).unwrap();
        let error = native.archive_preview(&store, "root").await.unwrap_err();
        assert!(error.to_string().contains(expected), "{mode}: {error:#}");
        assert!(store.pending_archives().unwrap().is_empty());
    }
    std::fs::write(&control, b"idle").unwrap();
    let preview = native.archive_preview(&store, "root").await.unwrap();
    assert_eq!(preview.session_ids, vec!["root", "child"]);
    std::fs::write(&control, b"changed").unwrap();
    assert_ne!(
        native
            .archive_preview(&store, "root")
            .await
            .unwrap()
            .fingerprint,
        preview.fingerprint
    );
    std::fs::write(&control, b"idle").unwrap();
    let claim = |id: &str, action| {
        store
            .receive_remote_command(
                &AgentCommand {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: "a".into(),
                    command_id: id.into(),
                    action,
                    created_at_unix: 10,
                    expires_at_unix: 1000,
                    payload: Some(json!({"session_id":"root","project_id":"p"})),
                },
                10,
            )
            .unwrap();
        store.mark_remote_accepted_reported(id, 10).unwrap();
        assert!(store.claim_remote_command(id, 11).unwrap());
    };
    claim("pending-input", CommandAction::CodexTurnStart);
    assert!(
        native
            .archive_preview(&store, "root")
            .await
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    store
        .finish_remote_command(
            "pending-input",
            farhelm_protocol::CommandState::Failed,
            None,
            Some("synthetic-cancel"),
            11,
        )
        .unwrap();
    store.create_schedule(&json!({"schedule_id":"later","project_id":"p","session_id":"root","prompt":"synthetic","trigger":{"type":"at_time","run_at_unix":100}}),10).unwrap();
    assert!(
        native
            .archive_preview(&store, "root")
            .await
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    store.cancel_schedule("later", 11).unwrap();
    // Unrelated reads/turns must not prevent archiving this idle subtree.
    let _unrelated = native.activity().await;
    claim("archive", CommandAction::CodexSessionArchive);
    native
        .archive_session(&store, "archive", "root", true, Some(&preview.fingerprint))
        .await
        .unwrap();
    assert!(store.check_session_writable("root").is_err());
    assert!(store.check_session_writable("child").is_err());
    assert!(store.pending_archives().unwrap().is_empty());
    assert!(store.create_schedule(&json!({"schedule_id":"denied","project_id":"p","session_id":"child","prompt":"synthetic","trigger":{"type":"at_time","run_at_unix":100}}),12).is_err());
    claim("restore", CommandAction::CodexSessionUnarchive);
    native
        .archive_session(&store, "restore", "root", false, None)
        .await
        .unwrap();
    store.check_session_writable("root").unwrap();
    assert!(store.check_session_writable("child").is_err());
    assert_eq!(
        store.remote_receipt("restore").unwrap().data.unwrap()["session_id"],
        "root"
    );
    assert!(
        !serde_json::to_string(&store.pending_events("a", 50).unwrap())
            .unwrap()
            .contains(root.to_str().unwrap())
    );
    drop(_unrelated);
    let preview = native.archive_preview(&store, "root").await.unwrap();
    claim("interrupted-archive", CommandAction::CodexSessionArchive);
    std::fs::write(&control, b"disconnect").unwrap();
    assert!(
        native
            .archive_session(
                &store,
                "interrupted-archive",
                "root",
                true,
                Some(&preview.fingerprint)
            )
            .await
            .is_err()
    );
    assert_eq!(store.pending_archives().unwrap().len(), 1);
    assert_eq!(store.orphan_running_remote_commands(20).unwrap(), 0);
    let reopened = ExperimentStore::open(&temp.path().join("agent.db")).unwrap();
    let resumed = Codex::new(Some(bin));
    resumed.recover_archives(&reopened).await.unwrap();
    assert_eq!(
        reopened
            .remote_receipt("interrupted-archive")
            .unwrap()
            .state,
        farhelm_protocol::CommandState::Completed
    );
    assert!(reopened.pending_archives().unwrap().is_empty());
    resumed.shutdown().await;
    native.shutdown().await;
}

fn fixture(directory: &std::path::Path, version: &str) -> PathBuf {
    let path = directory.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = '--version' ]; then printf 'codex-cli VERSION\n'; exit 0; fi
while IFS= read -r line; do
 id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
 [ -n "$id" ] || continue
 case "$line" in
  *'"method":"initialize"'*) result='{"userAgent":"fixture"}' ;;
  *'"method":"account/read"'*) result='{"requiresOpenaiAuth":false,"account":null}' ;;
  *'"method":"thread/list"'*)
   printf 'list\n' >> 'RECORD'
   case "$line" in *'"archived":true'*) result='{"data":[],"nextCursor":null}' ;; *) result='{"data":[{"id":"s","cwd":"/tmp/project","name":null,"preview":"首条用户请求","updatedAt":2}],"nextCursor":null}' ;; esac ;;
  *'"method":"thread/turns/list"'*)
   printf 'pages\n' >> 'RECORD'; sleep 0.03
   result='{"data":[{"id":"t1","status":"completed","items":[{"id":"u","type":"userMessage","content":[{"type":"text","text":"用户输入"}]},{"id":"i","type":"agentMessage","text":"**回复** https://example.org/a /private/file"}]}],"nextCursor":null}' ;;
  *'"method":"thread/read"'*)
   printf 'legacy\n' >> 'RECORD'; sleep 0.03
   result='{"thread":{"turns":[{"id":"t1","status":"completed","items":[{"id":"u","type":"userMessage","content":[{"type":"text","text":"用户输入"}]},{"id":"i","type":"agentMessage","text":"**回复** https://example.org/a /private/file"}]}]}}' ;;
  *'"method":"thread/start"'*) result='{"thread":{"id":"new","cwd":"/tmp/project","name":null,"turns":[]}}' ;;
  *'"method":"thread/resume"'*) printf '{"id":%s,"error":{"code":-32600,"message":"Empty sessions must use their loaded thread"}}\n' "$id"; continue ;;
  *'"method":"turn/start"'*)
   printf 'turn\n' >> 'RECORD'
   printf '{"id":%s,"result":{"turn":{"id":"active"}}}\n' "$id"
   case "$line" in *'"text":"crash"'*) exit 1 ;; esac
   printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"new","turnId":"active","itemId":"i1","delta":"第一段"}}' '{"method":"item/agentMessage/delta","params":{"threadId":"new","turnId":"active","itemId":"i2","delta":"第二段"}}' '{"method":"item/agentMessage/delta","params":{"threadId":"new","turnId":"active","itemId":"i1","delta":"结束"}}' '{"method":"turn/completed","params":{"threadId":"new","turn":{"id":"active","status":"completed"}}}'
   continue ;;
  *) result='{}' ;;
 esac
 printf '{"id":%s,"result":%s}\n' "$id" "$result"
done
"#;
    transport::write_fixture_executable(
        &path,
        &script
            .replace("VERSION", version)
            .replace("RECORD", directory.join("methods").to_str().unwrap()),
    );
    path
}

#[tokio::test]
async fn native_resume_inherits_settings_and_creation_retains_explicit_legacy_modes() {
    for version in ["0.147.0", "0.153.4"] {
        let directory = tempfile::tempdir().unwrap();
        let bin = fixture(directory.path(), version);
        let requests = directory.path().join("requests");
        let script = std::fs::read_to_string(&bin).unwrap();
        let mut lines = Vec::new();
        for line in script.lines() {
            if line.contains("Empty sessions must use their loaded thread") {
                lines.push("  *'\"method\":\"thread/resume\"'*) result='{\"thread\":{\"id\":\"s\",\"cwd\":\"/tmp/project\"},\"model\":\"gpt-5.4\",\"reasoningEffort\":\"high\",\"sandbox\":{\"type\":\"dangerFullAccess\"},\"approvalPolicy\":\"never\",\"approvalsReviewer\":\"user\"}' ;;".to_owned());
            } else {
                lines.push(line.to_owned());
                if line == " [ -n \"$id\" ] || continue" {
                    lines.push(format!(
                        " printf '%s\\n' \"$line\" >> '{}'",
                        requests.display()
                    ));
                }
            }
        }
        transport::write_fixture_executable(&bin, &lines.join("\n"));
        let native = Codex::new(Some(bin));
        native
            .call(
                "codex.session.resume",
                json!({"session_id":"s","cwd":"/tmp/project","mode":"inspect"}),
            )
            .await
            .unwrap();
        let page = native
            .call("codex.session.history", json!({"session_id":"s"}))
            .await
            .unwrap();
        assert_eq!(page["context"]["model"], "gpt-5.4");
        assert_eq!(page["context"]["sandbox"], "danger-full-access");
        assert_eq!(page["context"]["approval_policy"], "never");
        native
            .call(
                "codex.session.start",
                json!({"cwd":"/tmp/project","mode":"inspect","inherit_permissions":true}),
            )
            .await
            .unwrap();
        native
            .call(
                "codex.session.start",
                json!({"cwd":"/tmp/project","mode":"inspect"}),
            )
            .await
            .unwrap();
        native
            .call(
                "codex.session.start",
                json!({"cwd":"/tmp/project","mode":"edit"}),
            )
            .await
            .unwrap();
        let recorded = std::fs::read_to_string(requests)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let resume = recorded
            .iter()
            .find(|v| v["method"] == "thread/resume")
            .unwrap();
        for key in [
            "sandbox",
            "approvalPolicy",
            "model",
            "config",
            "permissionProfile",
        ] {
            assert!(
                resume["params"].get(key).is_none(),
                "resume must not override {key}"
            );
        }
        assert_eq!(
            resume["params"].get("excludeTurns").is_some(),
            version == "0.153.4"
        );
        let starts = recorded
            .iter()
            .filter(|v| v["method"] == "thread/start")
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 3);
        assert!(starts.iter().all(|v| v["params"]["ephemeral"] == false));
        assert!(
            recorded.iter().all(|v| v["method"] != "thread/name/set"),
            "new native sessions must not acquire a generic forced name"
        );
        assert!(starts[0]["params"].get("sandbox").is_none());
        assert!(starts[0]["params"].get("approvalPolicy").is_none());
        assert_eq!(starts[1]["params"]["sandbox"], "read-only");
        assert_eq!(starts[2]["params"]["sandbox"], "workspace-write");
        native.shutdown().await;
    }
}

#[tokio::test]
async fn model_choices_follow_native_catalog_and_never_override_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let bin = fixture(directory.path(), "0.153.4");
    let record = directory.path().join("model-requests");
    let script = std::fs::read_to_string(&bin).unwrap()
        .replace(" [ -n \"$id\" ] || continue", &format!(" [ -n \"$id\" ] || continue\n printf '%s\\n' \"$line\" >> '{}'", record.display()))
        .replace("  *) result='{}' ;;", r#"  *'"method":"model/list"'*)
   case "$line" in
    *'"cursor":"second"'*) result='{"data":[{"model":"local-fast","displayName":"Fast","supportedReasoningEfforts":[{"reasoningEffort":"low"}],"defaultReasoningEffort":"low","hidden":false}],"nextCursor":null}' ;;
    *) result='{"data":[{"model":"local-deep","displayName":"Deep","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],"defaultReasoningEffort":"medium","isDefault":true},{"model":"hidden","hidden":true},{"model":"/private/model"}],"nextCursor":"second"}' ;;
   esac ;;
  *) result='{}' ;;"#);
    transport::write_fixture_executable(&bin, &script);
    let native = Codex::new(Some(bin));
    native
        .call(
            "codex.session.start",
            json!({"cwd":"/tmp/project","mode":"inspect","inherit_permissions":true}),
        )
        .await
        .unwrap();
    let catalog = native.call("codex.models.list", json!({})).await.unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 2);
    let good = farhelm_protocol::CodexModelChoice {
        model: "local-deep".into(),
        reasoning_effort: "high".into(),
    };
    native
        .turn_with_model("new", "hello", "chosen", Some(&good), |_, _| async {
            Ok(())
        })
        .await
        .unwrap();
    for invalid in [
        farhelm_protocol::CodexModelChoice {
            model: "local-fast".into(),
            reasoning_effort: "high".into(),
        },
        farhelm_protocol::CodexModelChoice {
            model: "missing".into(),
            reasoning_effort: "low".into(),
        },
    ] {
        assert!(
            native
                .turn_with_model("new", "hello", "invalid", Some(&invalid), |_, _| async {
                    Ok(())
                })
                .await
                .unwrap_err()
                .to_string()
                .contains("model_choice_unavailable")
        );
    }
    native.shutdown().await;
    let requests: Vec<Value> = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(|v| serde_json::from_str(v).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|v| v["method"] == "model/list")
            .count(),
        2,
        "cached catalog must avoid redundant native reads"
    );
    let turns: Vec<_> = requests
        .iter()
        .filter(|v| v["method"] == "turn/start")
        .collect();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["params"]["model"], "local-deep");
    assert_eq!(turns[0]["params"]["effort"], "high");
    assert!(turns[0]["params"].get("sandboxPolicy").is_none());
    assert!(turns[0]["params"].get("approvalPolicy").is_none());
}

#[tokio::test]
async fn native_rename_updates_index_and_rejects_invalid_names() {
    let directory = tempfile::tempdir().unwrap();
    let native = Codex::new(Some(fixture(directory.path(), "0.153.4")));
    native
        .call(
            "codex.sessions.list",
            json!({"project_path":"/tmp/project","archived":"all"}),
        )
        .await
        .unwrap();
    native
        .call(
            "codex.session.rename",
            json!({"session_id":"s","name":"可在原生客户端找到的名称"}),
        )
        .await
        .unwrap();
    let listing = native
        .call(
            "codex.sessions.list",
            json!({"project_path":"/tmp/project","archived":"all"}),
        )
        .await
        .unwrap();
    assert_eq!(listing["sessions"][0]["title"], "可在原生客户端找到的名称");
    assert!(
        native
            .call(
                "codex.session.rename",
                json!({"session_id":"s","name":"Codex session"})
            )
            .await
            .is_err()
    );
    assert!(
        native
            .call(
                "codex.session.rename",
                json!({"session_id":"s","name":"/private/path"})
            )
            .await
            .is_err()
    );
    native.shutdown().await;
}

#[tokio::test]
async fn slow_optional_settings_do_not_hold_history_for_the_native_rpc_timeout() {
    let directory = tempfile::tempdir().unwrap();
    let bin = fixture(directory.path(), "0.153.4");
    let script = std::fs::read_to_string(&bin)
        .unwrap()
        .replace("printf 'legacy\\n' >>", "sleep 1; printf 'legacy\\n' >>");
    transport::write_fixture_executable(&bin, &script);
    let native = Codex::new(Some(bin));
    native.warm().await.unwrap();
    let start = Instant::now();
    let page = native
        .call("codex.session.history", json!({"session_id":"s"}))
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_millis(750));
    assert_eq!(page["turns"][0]["items"][0]["text"], "用户输入");
    assert_eq!(page["context"], json!({}));
    native.shutdown().await;
}

#[tokio::test]
#[ignore = "requires an installed Codex; creates and archives one synthetic local thread, no model turn"]
async fn installed_codex_preserves_native_permissions_and_model_on_resume() {
    let directory = tempfile::tempdir().unwrap();
    let bin = std::env::var_os("FARHELM_TEST_CODEX_BIN").map(PathBuf::from);
    let native = Codex::new(bin.clone());
    native.warm().await.unwrap();
    let connection = native.connection().await.unwrap();
    // Establish a native thread independently, as another Codex client would.
    let original = connection
        .request(
            "thread/start",
            json!({"cwd":directory.path(),"sandbox":"workspace-write","approvalPolicy":"never"}),
        )
        .await
        .unwrap();
    let id = original["thread"]["id"].as_str().unwrap();
    // Codex does not persist a thread until it has history. Inject one synthetic
    // history item without starting a model turn or executing any tools.
    connection
        .request(
            "thread/inject_items",
            json!({"threadId":id,"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"FarHelm synthetic settings verification."}]}]}),
        )
        .await
        .unwrap();
    // Archive flushes Codex's lazily materialized rollout. Restore visibility
    // before reconnecting so this checks a persisted thread, not a live cache.
    connection
        .request("thread/archive", json!({"threadId":id}))
        .await
        .unwrap();
    connection
        .request("thread/unarchive", json!({"threadId":id}))
        .await
        .unwrap();
    native.shutdown().await;
    // A fresh App Server must restore the saved settings, not its own defaults.
    let native = Codex::new(bin);
    native.warm().await.unwrap();
    let connection = native.connection().await.unwrap();
    let resumed = native
        .call(
            "codex.session.resume",
            json!({"session_id":id,"cwd":directory.path(),"mode":"inspect"}),
        )
        .await;
    let confirmed = native
        .inner
        .loaded
        .read()
        .await
        .get(id)
        .map(|thread| thread.context.clone());
    // Always clean up the thread before checking assertions, including adapter failures.
    let archived = connection
        .request("thread/archive", json!({"threadId":id}))
        .await;
    native.shutdown().await;
    resumed.unwrap();
    archived.unwrap();
    let confirmed = confirmed.unwrap();
    assert_eq!(confirmed, context::project(&original, None));
    assert_eq!(confirmed.sandbox.as_deref(), Some("workspace-write"));
    assert_eq!(confirmed.approval_policy.as_deref(), Some("never"));
    assert!(confirmed.model.is_some());
    println!("Native model and permissions preserved; no model turn executed.");
}

#[tokio::test]
async fn native_versions_share_index_and_coalesce_history_without_losing_text() {
    for version in ["0.147.0", "0.153.4"] {
        let directory = tempfile::tempdir().unwrap();
        let native = Codex::new(Some(fixture(directory.path(), version)));
        native.warm().await.unwrap();
        assert_eq!(native.status().state, "ready");
        assert_eq!(native.status().version.as_deref(), Some(version));
        let display = json!({"mode":"labels","session_ids":["s"],"bindings":[{"session_id":"s","project_id":"project"}]});
        for _ in 0..4 {
            native
                .call("codex.session.display", display.clone())
                .await
                .unwrap();
        }
        let pages = futures_util::future::join_all(
            (0..8).map(|_| native.call("codex.session.history", json!({"session_id":"s"}))),
        )
        .await;
        for page in pages {
            let page = page.unwrap();
            assert_eq!(
                page["turns"][0]["items"][1]["text"],
                "**回复** https://example.org/a [local path]"
            );
        }
        let methods = std::fs::read_to_string(directory.path().join("methods")).unwrap();
        assert_eq!(methods.lines().filter(|line| *line == "list").count(), 2);
        assert_eq!(
            methods
                .lines()
                .filter(|line| *line
                    == if version == "0.147.0" {
                        "legacy"
                    } else {
                        "pages"
                    })
                .count(),
            1
        );
        native.shutdown().await;
    }
}

fn gated_index_fixture(directory: &std::path::Path) -> PathBuf {
    let path = fixture(directory, "0.153.4");
    let script = std::fs::read_to_string(&path).unwrap();
    let gate = directory.join("hold-index");
    let fail = directory.join("fail-index");
    let script = script.replace(
        "   case \"$line\" in *'\"archived\":true'*)",
        &format!(
            "   while [ -f '{}' ]; do sleep 0.01; done\n   if [ -f '{}' ]; then printf '{{\"id\":%s,\"error\":{{\"code\":-32000,\"message\":\"fixture index unavailable\"}}}}\\n' \"$id\"; continue; fi\n   case \"$line\" in *'\"archived\":true'*)",
            gate.display(), fail.display()
        ),
    );
    assert!(script.contains("hold-index"));
    transport::write_fixture_executable(&path, &script);
    path
}

async fn wait_for_index_calls(directory: &std::path::Path, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let calls = std::fs::read_to_string(directory.join("methods"))
                .unwrap_or_default()
                .lines()
                .filter(|line| *line == "list")
                .count();
            if calls >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("fixture index call started");
}

#[tokio::test]
async fn warm_index_reads_do_not_wait_for_background_scan() {
    let directory = tempfile::tempdir().unwrap();
    let native = Codex::new(Some(gated_index_fixture(directory.path())));
    native.index(false).await.unwrap();
    // A completed snapshot remains usable when its next scan is already in flight.
    native.inner.index.write().await.refreshed = None;
    let gate = directory.path().join("hold-index");
    std::fs::write(&gate, "hold").unwrap();
    let refresh = tokio::spawn({
        let native = native.clone();
        async move { native.index(true).await }
    });
    wait_for_index_calls(directory.path(), 3).await;
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::remove_file(gate).unwrap();
    });
    let started = Instant::now();
    let display = native
        .call(
            "codex.session.display",
            json!({"mode":"labels","session_ids":["s"],"bindings":{"s":{"project_id":"project","cwd":"/tmp/project"}}}),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    println!(
        "warm_index_read_during_200ms_scan_ms={:.3}",
        elapsed.as_secs_f64() * 1000.0
    );
    release.await.unwrap();
    refresh.await.unwrap().unwrap();
    native.shutdown().await;
    assert_eq!(display["sessions"][0]["display_label"], "首条用户请求");
    assert!(
        elapsed < Duration::from_millis(100),
        "cached index waited for scan: {elapsed:?}"
    );
}

#[tokio::test]
async fn index_cold_reads_share_one_complete_scan() {
    let directory = tempfile::tempdir().unwrap();
    let native = Codex::new(Some(gated_index_fixture(directory.path())));
    let gate = directory.path().join("hold-index");
    std::fs::write(&gate, "hold").unwrap();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let native = native.clone();
        tasks.push(tokio::spawn(async move { native.index(false).await }));
    }
    wait_for_index_calls(directory.path(), 1).await;
    assert!(tasks.iter().all(|task| !task.is_finished()));
    std::fs::remove_file(gate).unwrap();
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap()[0]["id"], "s");
    }
    let methods = std::fs::read_to_string(directory.path().join("methods")).unwrap();
    assert_eq!(methods.lines().filter(|line| *line == "list").count(), 2);
    native.shutdown().await;
}

#[tokio::test]
async fn index_overlapping_forced_scans_coalesce_and_preserve_native_changes() {
    let directory = tempfile::tempdir().unwrap();
    let native = Codex::new(Some(gated_index_fixture(directory.path())));
    native.index(false).await.unwrap();
    let gate = directory.path().join("hold-index");
    std::fs::write(&gate, "hold").unwrap();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let native = native.clone();
        tasks.push(tokio::spawn(async move { native.index(true).await }));
    }
    wait_for_index_calls(directory.path(), 3).await;
    let connection = native.connection().await.unwrap();
    for event in [
        json!({"method":"thread/name/updated","params":{"threadId":"s","threadName":"新的正式标题"}}),
        json!({"method":"thread/status/changed","params":{"threadId":"s","status":{"type":"active"}}}),
        json!({"method":"thread/archived","params":{"threadId":"s"}}),
    ] {
        connection.events.send(event).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while native.inner.index.read().await.rows["s"]["archived"] != true {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(gate).unwrap();
    for task in tasks {
        let rows = task.await.unwrap().unwrap();
        assert_eq!(rows[0]["name"], "新的正式标题");
        assert_eq!(rows[0]["status"]["type"], "active");
        assert_eq!(rows[0]["archived"], true);
    }
    let methods = std::fs::read_to_string(directory.path().join("methods")).unwrap();
    assert_eq!(methods.lines().filter(|line| *line == "list").count(), 4);
    native.shutdown().await;
}

#[tokio::test]
async fn index_failed_and_cancelled_scans_keep_snapshot_and_allow_retry() {
    let directory = tempfile::tempdir().unwrap();
    let native = Codex::new(Some(gated_index_fixture(directory.path())));
    let snapshot = native.index(false).await.unwrap();
    let fail = directory.path().join("fail-index");
    std::fs::write(&fail, "fail").unwrap();
    native.inner.index.write().await.refreshed = None;
    assert!(native.index(true).await.is_err());
    assert_eq!(native.inner.index.read().await.rows["s"], snapshot[0]);
    assert!(native.inner.refresh.try_lock().is_ok());
    std::fs::remove_file(fail).unwrap();
    assert_eq!(native.index(false).await.unwrap(), snapshot);

    let gate = directory.path().join("hold-index");
    std::fs::write(&gate, "hold").unwrap();
    let refresh = tokio::spawn({
        let native = native.clone();
        async move { native.index(true).await }
    });
    wait_for_index_calls(directory.path(), 6).await;
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    assert!(native.inner.refresh.try_lock().is_ok());
    assert_eq!(native.inner.index.read().await.rows["s"], snapshot[0]);
    std::fs::remove_file(gate).unwrap();
    assert_eq!(native.index(true).await.unwrap(), snapshot);
    native.shutdown().await;
}

#[tokio::test]
async fn empty_complete_index_is_available_during_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let path = gated_index_fixture(directory.path());
    let script = std::fs::read_to_string(&path).unwrap().replace(
        r#"[{"id":"s","cwd":"/tmp/project","name":null,"preview":"首条用户请求","updatedAt":2}]"#,
        "[]",
    );
    transport::write_fixture_executable(&path, &script);
    let native = Codex::new(Some(path));
    assert!(native.index(false).await.unwrap().is_empty());
    native.inner.index.write().await.refreshed = None;
    let scan = native.inner.refresh.lock().await;
    let rows = tokio::time::timeout(Duration::from_millis(100), native.index(false)).await;
    drop(scan);
    native.shutdown().await;
    assert!(rows.unwrap().unwrap().is_empty());
}

#[tokio::test]
async fn loaded_empty_thread_can_send_and_interleaved_items_keep_offsets_before_terminal() {
    let directory = tempfile::tempdir().unwrap();
    let native = Codex::new(Some(fixture(directory.path(), "0.153.4")));
    let params = json!({"cwd":"/tmp/project","mode":"inspect","session_id":"new"});
    native
        .call("codex.session.start", params.clone())
        .await
        .unwrap();
    native
        .call("codex.session.resume", params.clone())
        .await
        .unwrap();
    let empty = native
        .call("codex.session.history", json!({"session_id":"new"}))
        .await
        .unwrap();
    assert_eq!(empty["turns"], json!([]));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    native
        .turn("new", "hello", "op", |kind, data| {
            events.lock().unwrap().push((kind, data));
            async { Ok(()) }
        })
        .await
        .unwrap();
    let events = events.lock().unwrap().clone();
    assert_eq!(events.first().unwrap().0, "codex.turn.started");
    assert_eq!(events.last().unwrap().0, "codex.turn.completed");
    let item: Vec<_> = events
        .iter()
        .filter(|(kind, data)| *kind == "codex.message.delta" && data["item_id"] == "i1")
        .map(|(_, data)| {
            (
                data["text_offset"].as_u64().unwrap(),
                data["delta"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(item, vec![(0, "第一段"), (3, "结束")]);
    drop(events);
    assert!(
        native
            .call(
                "codex.session.resume",
                json!({"cwd":"/unapproved","mode":"edit","session_id":"new"})
            )
            .await
            .is_err()
    );
    let error = native
        .turn("new", "crash", "op-2", |_, _| async { Ok(()) })
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<crate::CodexTurnOrphaned>().is_some());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("methods"))
            .unwrap()
            .lines()
            .filter(|line| *line == "turn")
            .count(),
        2
    );
    native.shutdown().await;
}

#[tokio::test]
async fn handoff_refuses_active_unsaved_and_background_work_then_closes_stdio() {
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path().join("codex");
    let row_path = directory.path().join("row");
    let terminal_path = directory.path().join("terminals");
    let rollout = directory.path().join("rollout");
    let closed = directory.path().join("closed");
    let mut row = json!({"thread":{"id":"s","cwd":"/tmp/project","path":rollout,"status":{"type":"idle"},"turns":[]}});
    std::fs::write(&row_path, row.to_string()).unwrap();
    std::fs::write(&terminal_path, r#"{"data":[],"nextCursor":null}"#).unwrap();
    transport::write_fixture_executable(
        &bin,
        &r#"#!/bin/sh
if [ "$1" = '--version' ]; then printf 'codex-cli 0.153.4\n'; exit 0; fi
while IFS= read -r line; do
 id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
 [ -n "$id" ] || continue
 case "$line" in
  *'"method":"initialize"'*) result='{"userAgent":"fixture"}' ;;
  *'"method":"account/read"'*) result='{"requiresOpenaiAuth":false,"account":null}' ;;
  *'"method":"thread/start"'*|*'"method":"thread/read"'*) result=$(cat 'ROW') ;;
  *'"method":"thread/loaded/list"'*) result='{"data":["s"],"nextCursor":null}' ;;
  *'"method":"thread/backgroundTerminals/list"'*) result=$(cat 'TERMINALS') ;;
  *) result='{}' ;;
 esac
 printf '{"id":%s,"result":%s}\n' "$id" "$result"
done
printf 'closed\n' >> 'CLOSED'
"#
        .replace("ROW", row_path.to_str().unwrap())
        .replace("TERMINALS", terminal_path.to_str().unwrap())
        .replace("CLOSED", closed.to_str().unwrap()),
    );
    let native = Codex::new(Some(bin));
    native
        .call(
            "codex.session.start",
            json!({"cwd":"/tmp/project","mode":"inspect"}),
        )
        .await
        .unwrap();
    let connection = native.connection().await.unwrap();
    let active = native.activity().await;
    assert!(
        native
            .handoff("s")
            .await
            .unwrap_err()
            .to_string()
            .contains("codex_handoff_busy")
    );
    drop(active);
    assert!(
        native
            .handoff("s")
            .await
            .unwrap_err()
            .to_string()
            .contains("codex_handoff_unsaved")
    );
    std::fs::write(&rollout, "synthetic").unwrap();
    row["thread"]["status"]["type"] = json!("active");
    std::fs::write(&row_path, row.to_string()).unwrap();
    assert!(
        native
            .handoff("s")
            .await
            .unwrap_err()
            .to_string()
            .contains("codex_handoff_busy")
    );
    row["thread"]["status"]["type"] = json!("idle");
    std::fs::write(&row_path, row.to_string()).unwrap();
    std::fs::write(
        &terminal_path,
        r#"{"data":[{"command":"PRIVATE_COMMAND","cwd":"PRIVATE_PATH"}],"nextCursor":null}"#,
    )
    .unwrap();
    let error = native.handoff("s").await.unwrap_err().to_string();
    assert_eq!(error, "codex_handoff_background");
    assert!(connection.alive.load(Ordering::Acquire));
    assert!(!closed.exists());
    std::fs::write(&terminal_path, r#"{"data":[],"nextCursor":null}"#).unwrap();
    assert_eq!(
        native.handoff("s").await.unwrap(),
        json!({"session_id":"s"})
    );
    assert!(!connection.alive.load(Ordering::Acquire));
    assert_eq!(std::fs::read_to_string(closed).unwrap(), "closed\n");
    // A duplicate handoff cannot restart a process or acquire a fresh native writer.
    assert_eq!(
        native.handoff("s").await.unwrap(),
        json!({"session_id":"s"})
    );
    assert!(native.inner.connection.lock().await.is_none());
}
