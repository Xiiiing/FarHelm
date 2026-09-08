use super::tests::{browser_cookie, test_state};
use super::*;
use farhelm_protocol::{
    AgentEvent, CommandState,
    live::{AgentFrame, CodexReadiness, HubFrame, LIVE_PROTOCOL},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const TOKEN: &str = "paired-live-fixture-token-1234567890";

async fn open(base: &str) -> Socket {
    let mut request = base.replace("http:", "ws:").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    request
        .headers_mut()
        .insert("sec-websocket-protocol", LIVE_PROTOCOL.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}
async fn send(socket: &mut Socket, frame: AgentFrame) {
    socket
        .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
        .await
        .unwrap();
}
async fn recv(socket: &mut Socket) -> HubFrame {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => break serde_json::from_str(&text).unwrap(),
                Message::Ping(value) => socket.send(Message::Pong(value)).await.unwrap(),
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    })
    .await
    .expect("live delivery must not await a polling interval")
}
async fn heartbeat(socket: &mut Socket) {
    send(
        socket,
        AgentFrame::Heartbeat {
            request_id: 0,
            heartbeat: AgentHeartbeat::new("gpu-a", "fixture", PRODUCT_VERSION),
            codex: CodexReadiness {
                state: "ready".into(),
                version: Some("0.153.4".into()),
                reason: None,
            },
        },
    )
    .await;
    assert!(matches!(
        recv(socket).await,
        HubFrame::Reply {
            request_id: 0,
            status: 200,
            ..
        }
    ));
}

#[tokio::test]
async fn live_commands_reads_receipts_replacement_and_transient_privacy() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("hub.db");
    let mut config = test_state().config.as_ref().clone();
    config.database_path = database.clone();
    config.agent_tokens.insert("gpu-a".into(), TOKEN.into());
    let state = AppState::new(config).unwrap();
    state
        .events
        .save_browser_session(
            &secret_hash("test-session"),
            "admin",
            "test-csrf",
            1,
            u64::MAX / 2,
        )
        .unwrap();
    let inspect = rusqlite::Connection::open(&database).unwrap();
    state.events.ingest("gpu-a", &[AgentEvent { protocol:FARHELM_PROTOCOL.into(), event_id:"session".into(), agent_id:"gpu-a".into(), sequence:1, event_type:"codex.session.updated".into(), created_at_unix:unix_time(), payload:json!({"session_id":"ses-live","project_id":"fixture","mode":"inspect","state":"idle","updated_at_unix":unix_time()}) }]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = app(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let abort = server.abort_handle();
    let client = reqwest::Client::new();
    let mut socket = open(&format!("{base}/api/v1/agent/connect")).await;
    heartbeat(&mut socket).await;
    let rows: serde_json::Value = client
        .get(format!("{base}/api/v1/agents"))
        .header("cookie", browser_cookie())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows["agents"][0]["codex"]["state"], "ready");
    let legacy = client
        .post(format!("{base}/api/v1/agent/commands/claim"))
        .bearer_auth(TOKEN)
        .json(&json!({"protocol":FARHELM_PROTOCOL,"agent_id":"gpu-a"}))
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), reqwest::StatusCode::CONFLICT);
    let submit_client = client.clone();
    let submit_base = base.clone();
    let submit = tokio::spawn(async move {
        submit_client
            .post(format!(
                "{submit_base}/api/v1/codex/sessions/ses-live/messages"
            ))
            .header("cookie", browser_cookie())
            .header("x-csrf-token", "test-csrf")
            .header("idempotency-key", "live-operation-00001")
            .json(&json!({"prompt":"SYNTHETIC_PRIVATE_PROMPT_080","delivery":"queue","model_choice":{"model":"synthetic-local-model","reasoning_effort":"high"}}))
            .send()
            .await
            .unwrap()
    });
    let HubFrame::Command { command } = recv(&mut socket).await else {
        panic!("command expected")
    };
    assert_eq!(
        command.payload.as_ref().unwrap()["prompt"],
        "SYNTHETIC_PRIVATE_PROMPT_080"
    );
    assert_eq!(
        command.payload.as_ref().unwrap()["model_choice"],
        json!({"model":"synthetic-local-model","reasoning_effort":"high"})
    );
    let encoded: String = inspect
        .query_row(
            "SELECT payload_json FROM typed_commands WHERE command_id=?1",
            [&command.command_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!encoded.contains("SYNTHETIC_PRIVATE_PROMPT_080"));
    assert!(!encoded.contains("synthetic-local-model"));
    let report = CommandReportRequest {
        protocol: FARHELM_PROTOCOL.into(),
        agent_id: "gpu-a".into(),
        command_id: command.command_id.clone(),
        state: CommandState::Accepted,
        result: None,
        data: None,
        detail: None,
    };
    send(
        &mut socket,
        AgentFrame::CommandReport {
            request_id: 1,
            report: report.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut socket).await,
        HubFrame::Reply {
            request_id: 1,
            status: 200,
            ..
        }
    ));
    assert_eq!(
        submit.await.unwrap().status(),
        reqwest::StatusCode::ACCEPTED
    );
    send(
        &mut socket,
        AgentFrame::CommandReport {
            request_id: 2,
            report,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut socket).await,
        HubFrame::Reply {
            request_id: 2,
            status: 200,
            ..
        }
    ));
    let reader_client = client.clone();
    let reader_base = base.clone();
    let read = tokio::spawn(async move {
        reader_client
            .get(format!(
                "{reader_base}/api/v1/codex/sessions/ses-live/transcript"
            ))
            .header("cookie", browser_cookie())
            .send()
            .await
            .unwrap()
    });
    let HubFrame::Read {
        request,
        expires_at_unix,
    } = recv(&mut socket).await
    else {
        panic!("read expected")
    };
    assert!(expires_at_unix <= unix_time() + 20);
    let history = json!({"session_id":"ses-live","turns":[{"turn_id":"turn-live","status":"completed","items":[{"item_id":"item-live","kind":"assistant_message","text":"SYNTHETIC_PRIVATE_HISTORY_080"}]}]});
    send(
        &mut socket,
        AgentFrame::ReadReport {
            request_id: 3,
            report: AgentReadReportRequest {
                protocol: FARHELM_PROTOCOL.into(),
                agent_id: "gpu-a".into(),
                request_id: request.request_id,
                ok: true,
                data: Some(history),
                detail: None,
            },
        },
    )
    .await;
    assert!(matches!(
        recv(&mut socket).await,
        HubFrame::Reply {
            request_id: 3,
            status: 204,
            ..
        }
    ));
    assert!(
        read.await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("SYNTHETIC_PRIVATE_HISTORY_080")
    );
    let mut events = state.event_bus.subscribe();
    send(&mut socket, AgentFrame::Delta { event:AgentEvent { protocol:FARHELM_PROTOCOL.into(), event_id:"delta-live".into(), agent_id:"gpu-a".into(), sequence:0, event_type:"codex.message.delta".into(), created_at_unix:unix_time(), payload:json!({"session_id":"ses-live","data":{"turn_id":"turn-live","item_id":"item-live","text_offset":0,"delta":"SYNTHETIC_PRIVATE_DELTA_080","cwd":"/private/fixture"}}) } }).await;
    let delta = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.event_type, "codex.message.delta");
    assert!(delta.payload["data"].get("cwd").is_none());
    assert_eq!(
        inspect
            .query_row(
                "SELECT count(*) FROM agent_events WHERE event_id='delta-live'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let generation = state
        .live
        .read()
        .await
        .get("gpu-a")
        .unwrap()
        .generation
        .clone();
    let mut replacement = open(&format!("{base}/api/v1/agent/connect")).await;
    heartbeat(&mut replacement).await;
    assert_ne!(
        state.live.read().await.get("gpu-a").unwrap().generation,
        generation
    );
    assert_eq!(state.live.read().await.len(), 1);
    assert!(state.agents.read().await["gpu-a"].last_seen_unix > 0);
    replacement.close(None).await.unwrap();
    abort.abort();
}

#[tokio::test]
async fn live_handshake_rejects_browser_origin_wrong_credentials_and_unversioned_clients() {
    let mut config = test_state().config.as_ref().clone();
    config.agent_tokens.insert("gpu-a".into(), TOKEN.into());
    let state = AppState::new(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/api/v1/agent/connect",
        listener.local_addr().unwrap()
    );
    let app = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for (token, protocol, origin, expected) in [
        ("invalid-token", true, false, 401),
        (TOKEN, false, false, 400),
        (TOKEN, true, true, 403),
    ] {
        let mut request = url.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        if protocol {
            request
                .headers_mut()
                .insert("sec-websocket-protocol", LIVE_PROTOCOL.parse().unwrap());
        }
        if origin {
            request
                .headers_mut()
                .insert("origin", "https://untrusted.example".parse().unwrap());
        }
        let error = tokio_tungstenite::connect_async(request).await.unwrap_err();
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("expected HTTP rejection")
        };
        assert_eq!(response.status().as_u16(), expected);
    }
    assert!(state.live.read().await.is_empty());
    server.abort();
}
