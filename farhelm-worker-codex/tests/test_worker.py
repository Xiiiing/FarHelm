from __future__ import annotations

import io

from farhelm_worker_codex.__main__ import run
from farhelm_worker_codex.framing import read_frame, write_frame
from farhelm_worker_codex.worker import WORKER_PROTOCOL, handle_request


def hello_request() -> dict[str, object]:
    return {
        "protocol": WORKER_PROTOCOL,
        "kind": "request",
        "request_id": "req_test",
        "method": "worker.hello",
        "params": {"agent_version": "0.1.0"},
    }


def test_worker_hello_advertises_only_implemented_capability() -> None:
    response = handle_request(hello_request())
    assert response["ok"] is True
    assert response["request_id"] == "req_test"
    assert response["result"] == {
        "worker": "farhelm-worker-codex",
        "version": "0.1.0",
        "capabilities": ["worker.hello"],
    }


def test_unknown_method_returns_structured_error() -> None:
    request = hello_request()
    request["method"] = "turn.start"
    response = handle_request(request)
    assert response["ok"] is False
    assert response["error"] == {
        "code": "method_not_found",
        "message": "unsupported method: turn.start",
    }


def test_stdio_loop_stops_cleanly_at_eof() -> None:
    source = io.BytesIO()
    write_frame(source, hello_request())
    source.seek(0)
    output = io.BytesIO()

    assert run(source, output) == 0
    output.seek(0)
    response = read_frame(output)
    assert response is not None
    assert response["ok"] is True
