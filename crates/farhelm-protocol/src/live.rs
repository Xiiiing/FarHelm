//! FarHelm's authenticated, outbound Agent channel. Vendor RPC is never forwarded.
use crate::{
    AgentCommand, AgentEvent, AgentEventBatch, AgentHeartbeat, AgentReadReportRequest,
    AgentReadRequest, CommandReportRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const LIVE_PROTOCOL: &str = "farhelm/1.live";
pub const LIVE_FRAME_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexReadiness {
    pub state: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentFrame {
    Heartbeat {
        request_id: u64,
        heartbeat: AgentHeartbeat,
        codex: CodexReadiness,
    },
    Events {
        request_id: u64,
        batch: AgentEventBatch,
    },
    CommandReport {
        request_id: u64,
        report: CommandReportRequest,
    },
    ReadReport {
        request_id: u64,
        report: AgentReadReportRequest,
    },
    Delta {
        event: AgentEvent,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HubFrame {
    Reply {
        request_id: u64,
        status: u16,
        data: Value,
    },
    Command {
        command: AgentCommand,
    },
    Read {
        request: AgentReadRequest,
        expires_at_unix: u64,
    },
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_frames_are_versioned_without_vendor_rpc_escape_hatch() {
        let frame: HubFrame =
            serde_json::from_str(include_str!("../tests/fixtures/live-channel.json")).unwrap();
        assert!(
            matches!(frame,HubFrame::Read {ref request,..} if request.method=="codex.session.history")
        );
        assert!(
            serde_json::from_value::<AgentFrame>(
                serde_json::json!({"kind":"vendor_rpc","method":"command/exec"})
            )
            .is_err()
        );
        assert_eq!(LIVE_PROTOCOL, "farhelm/1.live");
    }
}
