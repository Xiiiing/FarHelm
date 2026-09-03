# farhelm-worker-codex

Internal Python adapter managed by `farhelm-agent`. The `V0.2.0` single-file installation release implements only the versioned `worker.hello` handshake over framed stdin/stdout. It does not connect to the Codex SDK or expose a network service.

Protocol output is written exclusively to stdout; diagnostics are written to stderr.
