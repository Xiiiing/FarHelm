# farhelm-worker-codex

Internal Python adapter embedded and managed by `farhelm-agent`. V0.4.0 pins `openai-codex==0.147.0` and adapts thread list/start/resume plus turn start/steer/interrupt and streamed events over framed stdin/stdout. It exposes no network service.

Protocol output is written exclusively to stdout; diagnostics are written to stderr.
