//! One reconnecting outbound connection; commands are persisted by the receiver.
use anyhow::{Context, Result, ensure};
use farhelm_protocol::{
    AgentCommand, AgentHeartbeat, AgentReadRequest,
    live::{AgentFrame, CodexReadiness, HubFrame, LIVE_FRAME_BYTES, LIVE_PROTOCOL},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{
    Message, client::IntoClientRequest, protocol::WebSocketConfig,
};

type Waiters = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
#[derive(Clone)]
pub struct Link {
    outgoing: Arc<Mutex<Option<mpsc::Sender<AgentFrame>>>>,
    pending: Waiters,
    next: Arc<AtomicU64>,
    pub connected: watch::Sender<bool>,
    reconnect: Arc<tokio::sync::Notify>,
}
impl Default for Link {
    fn default() -> Self {
        Self {
            outgoing: Arc::default(),
            pending: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
            connected: watch::channel(false).0,
            reconnect: Arc::new(tokio::sync::Notify::new()),
        }
    }
}
impl Link {
    pub fn reset(&self) {
        self.reconnect.notify_one();
    }
    pub async fn request(&self, frame: impl FnOnce(u64) -> AgentFrame) -> Result<Value> {
        let sender = self
            .outgoing
            .lock()
            .expect("live sender poisoned")
            .clone()
            .context("hub_disconnected")?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("live replies poisoned");
            ensure!(pending.len() < 64, "live_capacity");
            pending.insert(id, reply);
        }
        let _guard = WaiterGuard {
            waiters: self.pending.clone(),
            id,
        };
        tokio::time::timeout(Duration::from_secs(18), async {
            sender.send(frame(id)).await.context("hub_disconnected")?;
            receiver
                .await
                .context("hub_disconnected")?
                .map_err(anyhow::Error::msg)
        })
        .await
        .context("hub_reply_timeout")?
    }
    pub fn delta(&self, event: farhelm_protocol::AgentEvent) {
        // Deltas can be reconstructed from history. Never persist transcript text.
        if let Some(sender) = self.outgoing.lock().expect("live sender poisoned").as_ref()
            && sender.try_send(AgentFrame::Delta { event }).is_err()
        {
            // Reconnection prompts an authoritative history reconciliation.
            // Native execution and durable receipts remain independent.
            self.reset();
        }
    }
    fn disconnect(&self) {
        self.outgoing.lock().expect("live sender poisoned").take();
        self.connected.send_replace(false);
        for (_, reply) in std::mem::take(&mut *self.pending.lock().expect("live replies poisoned"))
        {
            let _ = reply.send(Err("hub_disconnected".into()));
        }
    }
}
struct WaiterGuard {
    waiters: Waiters,
    id: u64,
}
impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.remove(&self.id);
        }
    }
}

pub async fn run(
    hub: crate::HubArgs,
    heartbeat: AgentHeartbeat,
    codex: crate::codex::Codex,
    commands: mpsc::Sender<AgentCommand>,
    reads: mpsc::Sender<(AgentReadRequest, u64)>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut delay = 1u64;
    loop {
        let started = std::time::Instant::now();
        let result = connection(&hub, &heartbeat, &codex, &commands, &reads, &wake).await;
        hub.link.disconnect();
        if result.is_err() {
            tracing::warn!("Hub live connection unavailable; queued local work is retained");
        }
        if started.elapsed() > Duration::from_secs(30) {
            delay = 1;
        }
        tokio::time::sleep(Duration::from_millis(
            delay * 1000 + rand::random_range(0..500),
        ))
        .await;
        delay = (delay * 2).min(30);
    }
}

async fn connection(
    hub: &crate::HubArgs,
    heartbeat: &AgentHeartbeat,
    codex: &crate::codex::Codex,
    commands: &mpsc::Sender<AgentCommand>,
    reads: &mpsc::Sender<(AgentReadRequest, u64)>,
    wake: &tokio::sync::Notify,
) -> Result<()> {
    let mut url = crate::hub_endpoint(&hub.hub, "/api/v1/agent/connect")?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid_hub_scheme"))?;
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {}", hub.token).parse()?);
    request
        .headers_mut()
        .insert("sec-websocket-protocol", LIVE_PROTOCOL.parse()?);
    let configuration = WebSocketConfig::default()
        .max_message_size(Some(LIVE_FRAME_BYTES))
        .max_frame_size(Some(LIVE_FRAME_BYTES));
    let (socket, response) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(request, Some(configuration), true),
    )
    .await
    .context("hub_connect_timeout")??;
    ensure!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .is_some_and(|p| p == LIVE_PROTOCOL),
        "hub_upgrade_required"
    );
    let (mut sender, mut receiver) = socket.split();
    let (outgoing, mut queued) = mpsc::channel::<AgentFrame>(128);
    let mut status = codex.subscribe_status();
    let heartbeat_frame = |status: crate::codex::CodexStatus| AgentFrame::Heartbeat {
        request_id: 0,
        heartbeat: heartbeat.clone(),
        codex: CodexReadiness {
            state: status.state,
            version: status.version,
            reason: status.reason,
        },
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        sender.send(Message::Text(
            serde_json::to_string(&heartbeat_frame(codex.status()))?.into(),
        )),
    )
    .await??;
    // Do not expose the sender until Hub has acknowledged the initial heartbeat.
    let mut initialized = false;
    let mut ticker = tokio::time::interval(Duration::from_secs(15));
    ticker.tick().await;
    let mut last_received = std::time::Instant::now();
    let initialize_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(initialize_deadline), if !initialized => anyhow::bail!("hub_heartbeat_timeout"),
            _ = hub.link.reconnect.notified() => break,
            message=receiver.next()=>{
                let Some(message)=message else {break};let message=message?;last_received=std::time::Instant::now();
                match message {
                    Message::Text(text)=>match serde_json::from_str::<HubFrame>(&text).context("invalid_hub_frame")? {
                        HubFrame::Reply {request_id,status,data}=>{
                            if request_id==0 {
                                ensure!((200..300).contains(&status),"hub_heartbeat_rejected");
                                if !initialized {initialized=true;*hub.link.outgoing.lock().expect("live sender poisoned")=Some(outgoing.clone());hub.link.connected.send_replace(true);wake.notify_one();}
                            } else if let Some(reply)=hub.link.pending.lock().expect("live replies poisoned").remove(&request_id) {
                                let _=reply.send(if (200..300).contains(&status) {Ok(data)} else {Err(format!("hub_request_rejected:{status}"))});
                            }
                        },
                        HubFrame::Command {command}=>{ensure!(initialized,"hub_heartbeat_missing");commands.try_send(command).context("command_queue_full")?;},
                        HubFrame::Read {request,expires_at_unix}=>{ensure!(initialized,"hub_heartbeat_missing");reads.try_send((request,expires_at_unix)).context("read_queue_full")?;},
                    },
                    Message::Ping(bytes)=>{tokio::time::timeout(Duration::from_secs(5), sender.send(Message::Pong(bytes))).await??;},
                    Message::Pong(_)=>{},
                    Message::Close(_)=>break,
                    _=>anyhow::bail!("unsupported_hub_frame"),
                }
            },
            frame=queued.recv()=>{
                let Some(frame)=frame else {break};let text=serde_json::to_string(&frame)?;
                ensure!(text.len()<=LIVE_FRAME_BYTES,"live_frame_too_large");
                tokio::time::timeout(Duration::from_secs(5),sender.send(Message::Text(text.into()))).await??;
            },
            _=status.changed()=>{
                tokio::time::timeout(Duration::from_secs(5), sender.send(Message::Text(serde_json::to_string(&heartbeat_frame(codex.status()))?.into()))).await??;
            },
            _=ticker.tick()=>{
                ensure!(last_received.elapsed()<Duration::from_secs(45),"hub_heartbeat_timeout");
                tokio::time::timeout(Duration::from_secs(5), sender.send(Message::Text(serde_json::to_string(&heartbeat_frame(codex.status()))?.into()))).await??;
            }
        }
    }
    Ok(())
}
