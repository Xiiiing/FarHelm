use super::*;
use std::os::unix::fs::PermissionsExt;

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
    std::fs::write(
        &path,
        script
            .replace("VERSION", version)
            .replace("RECORD", directory.join("methods").to_str().unwrap()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
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
