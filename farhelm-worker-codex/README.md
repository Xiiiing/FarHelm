# farhelm-worker-codex

Internal Python adapter managed by `farhelm-agent`. The `0.1.0` skeleton implements only the versioned `worker.hello` handshake over framed stdin/stdout. It does not connect to the Codex SDK or expose a network service.

Protocol output is written exclusively to stdout; diagnostics are written to stderr.
