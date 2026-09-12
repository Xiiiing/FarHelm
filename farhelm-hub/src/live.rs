use super::*;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use farhelm_protocol::live::{
    AgentFrame, CodexReadiness, HubFrame, LIVE_FRAME_BYTES, LIVE_PROTOCOL,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;

#[derive(Clone)]
pub(super) struct LiveConnection {
    pub generation: String,
    pub stop: watch::Sender<bool>,
}

pub(super) async fn connect(
    State(state): State<AppState>,
    Extension(identity): Extension<AgentIdentity>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let AgentIdentity::Dedicated(agent) = identity else {
        return api_error(StatusCode::FORBIDDEN, "dedicated_agent_token_required");
    };
    if headers.contains_key(header::ORIGIN) {
        return api_error(StatusCode::FORBIDDEN, "agent_origin_rejected");
    }
    let has_protocol = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|s| s.trim() == LIVE_PROTOCOL));
    if !has_protocol {
        return api_error(StatusCode::BAD_REQUEST, "live_protocol_required");
    }
    upgrade
        .protocols([LIVE_PROTOCOL])
        .max_frame_size(LIVE_FRAME_BYTES)
        .max_message_size(LIVE_FRAME_BYTES)
        .on_upgrade(move |socket| serve(state, agent, socket))
}

async fn serve(state: AppState, agent: String, socket: WebSocket) {
    let generation = random_token();
    let (stop, mut stopped) = watch::channel(false);
    let lease = LiveConnection {
        generation: generation.clone(),
        stop,
    };
    if let Some(previous) = state.live.write().await.insert(agent.clone(), lease) {
        previous.stop.send_replace(true);
    }
    if channel(&state, &agent, &generation, socket, &mut stopped)
        .await
        .is_err()
    {
        tracing::info!(agent_id=%agent,"Agent live channel closed; reconnect required");
    }
    let removed = {
        let mut connections = state.live.write().await;
        if connections
            .get(&agent)
            .is_some_and(|c| c.generation == generation)
        {
            connections.remove(&agent);
            // Update the previous generation before a replacement can initialize.
            if let Some(stored) = state.agents.write().await.get_mut(&agent) {
                stored.last_seen_unix = 0;
            }
            true
        } else {
            false
        }
    };
    if removed {
        broadcast_agent(&state, &agent).await;
    }
}

async fn channel(
    state: &AppState,
    agent: &str,
    generation: &str,
    socket: WebSocket,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let read_notify = {
        let mut broker = state.read_broker.lock().await;
        broker
            .notifies
            .entry(agent.to_owned())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    };
    let mut inflight = None::<String>;
    let mut receipt_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut initialized = false;
    let mut last_seen = std::time::Instant::now();
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        let command_wake = state.command_notify.notified();
        let read_wake = read_notify.notified();
        tokio::pin!(command_wake, read_wake);
        command_wake.as_mut().enable();
        read_wake.as_mut().enable();
        if *stop.borrow() {
            break;
        }
        if initialized {
            if inflight.is_none()
                && let Some(command) =
                    claim_command_now(state, &AgentIdentity::Dedicated(agent.to_owned()), agent)
                        .await?
            {
                inflight = Some(command.command_id.clone());
                receipt_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                send(&mut sender, &HubFrame::Command { command }).await?;
            }
            let requests = {
                let mut broker = state.read_broker.lock().await;
                let mut queue = broker.queues.remove(agent).unwrap_or_default();
                let mut requests = Vec::new();
                while let Some(request) = queue.pop_front() {
                    if let Some((_, reply, created)) = broker.waiters.get(&request.request_id)
                        && !reply.is_closed()
                        && created.elapsed() < Duration::from_secs(20)
                    {
                        requests.push((request, unix_time() + 20 - created.elapsed().as_secs()));
                    }
                }
                requests
            };
            for (request, expires_at_unix) in requests {
                send(
                    &mut sender,
                    &HubFrame::Read {
                        request,
                        expires_at_unix,
                    },
                )
                .await?;
            }
        }
        tokio::select! {
            _=tokio::time::sleep_until(receipt_deadline), if inflight.is_some()=>anyhow::bail!("agent_receipt_timeout"),
            _=stop.changed()=>break,
            _=command_wake=>{},
            _=read_wake=>{},
            _=ping.tick()=>{
                ensure!(last_seen.elapsed()<Duration::from_secs(45),"agent_heartbeat_timeout");
                tokio::time::timeout(Duration::from_secs(5),sender.send(Message::Ping(Vec::new().into()))).await??;
            },
            message=receiver.next()=>{
                let Some(message)=message else {break};
                let message=message?;
                if matches!(message,Message::Pong(_)|Message::Ping(_)) {last_seen=std::time::Instant::now();continue;}
                let Message::Text(text)=message else {break};
                let frame:AgentFrame=serde_json::from_str(&text).context("invalid_agent_frame")?;
                ensure!(state.live.read().await.get(agent).is_some_and(|c|c.generation==generation),"replaced_connection");
                last_seen=std::time::Instant::now();
                if let AgentFrame::Delta {event}=frame {
                    ensure!(initialized && event.agent_id==agent && event.protocol==FARHELM_PROTOCOL && matches!(event.event_type.as_str(),"codex.message.delta"|"codex.native.changed"),"invalid_delta");
                    let mut payload=farhelm_protocol::public_event_payload(&event.event_type,&event.payload);
                    payload["agent_id"]=serde_json::json!(agent);
                    let _=state.event_bus.send(StoredEvent {sequence:0,event_id:event.event_id,event_type:event.event_type,payload});
                    continue;
                }
                let (id,response)=match frame {
                    AgentFrame::Heartbeat {request_id,heartbeat,codex}=>{
                        ensure!(heartbeat.agent_id==agent,"agent_scope");validate_readiness(&codex)?;
                        let response=agent_heartbeat(State(state.clone()),Extension(AgentIdentity::Dedicated(agent.into())),Json(heartbeat)).await.into_response();
                        if response.status().is_success() {
                            if !initialized {
                                let _ = state.event_bus.send(StoredEvent { sequence:0, event_id:format!("agent-reconnected:{generation}"), event_type:"codex.stream.resync".into(), payload:serde_json::json!({"agent_id":agent}) });
                            }
                            initialized=true;
                            if let Some(stored)=state.agents.write().await.get_mut(agent) {stored.codex=Some(codex);}
                            broadcast_agent(state,agent).await;
                        }
                        (request_id,response)
                    },
                    AgentFrame::Events {request_id,batch}=>{
                        ensure!(initialized,"heartbeat_required");
                        (request_id,agent_events(State(state.clone()),Extension(AgentIdentity::Dedicated(agent.into())),Json(batch)).await)
                    },
                    AgentFrame::CommandReport {request_id,report}=>{
                        ensure!(initialized,"heartbeat_required");
                        let command=report.command_id.clone();
                        let response=report_command(State(state.clone()),Extension(AgentIdentity::Dedicated(agent.into())),Json(report)).await;
                        if response.status().is_success() && inflight.as_deref()==Some(&command) {inflight=None;}
                        (request_id,response)
                    },
                    AgentFrame::ReadReport {request_id,report}=>{
                        ensure!(initialized,"heartbeat_required");
                        (request_id,report_agent_read(State(state.clone()),Extension(AgentIdentity::Dedicated(agent.into())),Path(report.request_id.clone()),Json(report)).await)
                    },
                    AgentFrame::Delta {..}=>unreachable!(),
                };
                let status=response.status().as_u16();
                let bytes=axum::body::to_bytes(response.into_body(),LIVE_FRAME_BYTES).await?;
                let data=if bytes.is_empty() {serde_json::Value::Null} else {serde_json::from_slice(&bytes)?};
                send(&mut sender,&HubFrame::Reply {request_id:id,status,data}).await?;
            }
        }
    }
    Ok(())
}

async fn send(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    frame: &HubFrame,
) -> Result<()> {
    let text = serde_json::to_string(frame)?;
    ensure!(text.len() <= LIVE_FRAME_BYTES, "live_frame_too_large");
    tokio::time::timeout(
        Duration::from_secs(5),
        sender.send(Message::Text(text.into())),
    )
    .await??;
    Ok(())
}
fn validate_readiness(status: &CodexReadiness) -> Result<()> {
    ensure!(
        ["starting", "ready", "login_required", "unavailable"].contains(&status.state.as_str()),
        "invalid_readiness"
    );
    ensure!(
        status.version.as_ref().is_none_or(|s| s.len() <= 32
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))),
        "invalid_codex_version"
    );
    ensure!(
        status.reason.as_ref().is_none_or(
            |s| s.len() <= 128 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        ),
        "invalid_codex_reason"
    );
    Ok(())
}
pub(super) async fn broadcast_agent(state: &AppState, agent: &str) {
    if let Some(stored) = state.agents.read().await.get(agent) {
        let _=state.event_bus.send(StoredEvent {sequence:0,event_id:format!("agent:{agent}"),event_type:"agent.status".into(),payload:serde_json::json!({"agent_id":agent,"hostname":stored.hostname,"agent_version":stored.agent_version,"last_seen_unix":stored.last_seen_unix,"online":is_online(unix_time(),stored.last_seen_unix),"credential_state":stored.credential_state,"codex":stored.codex,"capabilities":stored.capabilities})});
    }
}
pub(super) fn broadcast_command(
    state: &AppState,
    command: &farhelm_protocol::CommandStatusResponse,
) {
    state.receipt_notify.notify_waiters();
    let _ = state.event_bus.send(StoredEvent {
        sequence: 0,
        event_id: command.command_id.clone(),
        event_type: "command.updated".into(),
        payload: serde_json::to_value(command).expect("command metadata"),
    });
}
