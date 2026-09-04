from __future__ import annotations

import io
import threading
from collections.abc import Mapping
from typing import Any

from farhelm_worker_codex.__main__ import run
from farhelm_worker_codex.framing import read_frame, write_frame
from farhelm_worker_codex.worker import (
    CAPABILITIES,
    WORKER_PROTOCOL,
    Emit,
    _snapshot_agent_text,
    handle_request,
)


def hello_request() -> dict[str, object]:
    return {
        "protocol": WORKER_PROTOCOL,
        "kind": "request",
        "request_id": "req_test",
        "method": "worker.hello",
        "params": {"agent_version": "0.3.0"},
    }


def test_worker_hello_advertises_implemented_capabilities() -> None:
    response = handle_request(hello_request())
    assert response["ok"] is True
    assert response["request_id"] == "req_test"
    assert response["result"] == {
        "worker": "farhelm-worker-codex",
        "version": "0.4.0",
        "capabilities": CAPABILITIES,
    }


def test_unknown_method_returns_structured_error() -> None:
    request = hello_request()
    request["method"] = "arbitrary.shell"
    response = handle_request(request)
    assert response["ok"] is False
    assert response["error"] == {
        "code": "method_not_found",
        "message": "unsupported method: arbitrary.shell",
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


def test_stdio_loop_can_interrupt_an_active_turn() -> None:
    started = threading.Event()
    interrupted = threading.Event()

    class BlockingBackend(FakeBackend):
        def turn_start(
            self, session_id: str, prompt: str, idempotency_key: str, emit: Emit
        ) -> Mapping[str, Any]:
            started.set()
            assert interrupted.wait(timeout=2)
            return {"session_id": session_id, "turn_id": "turn_1", "status": "interrupted"}

        def turn_interrupt(self, session_id: str, turn_id: str) -> Mapping[str, Any]:
            assert started.wait(timeout=2)
            interrupted.set()
            return {"session_id": session_id, "turn_id": turn_id, "interrupted": True}

    turn = request(
        "codex.turn.start",
        {"session_id": "ses", "prompt": "wait", "idempotency_key": "turn-command"},
    )
    turn["request_id"] = "turn-command"
    interrupt = request("codex.turn.interrupt", {"session_id": "ses", "turn_id": "turn_1"})
    interrupt["request_id"] = "interrupt-command"
    source = io.BytesIO()
    write_frame(source, turn)
    write_frame(source, interrupt)
    source.seek(0)
    output = io.BytesIO()

    assert run(source, output, BlockingBackend()) == 0
    output.seek(0)
    responses = []
    while frame := read_frame(output):
        if frame.get("kind") == "response":
            responses.append(frame)
    assert {response["request_id"] for response in responses} == {
        "turn-command",
        "interrupt-command",
    }


class FakeBackend:
    def sessions_list(self, project_path: str) -> Mapping[str, Any]:
        return {"sessions": [{"session_id": "ses_old", "cwd": project_path}]}

    def session_start(self, cwd: str, mode: str) -> Mapping[str, Any]:
        return {"session_id": "ses_new", "cwd": cwd, "mode": mode}

    def session_resume(self, session_id: str, cwd: str, mode: str) -> Mapping[str, Any]:
        return {"session_id": session_id, "cwd": cwd, "mode": mode}

    def turn_start(
        self, session_id: str, prompt: str, idempotency_key: str, emit: Emit
    ) -> Mapping[str, Any]:
        emit({"event": "codex.message.delta", "data": {"delta": "done"}})
        return {"session_id": session_id, "turn_id": "turn_1", "status": "completed"}

    def turn_steer(self, session_id: str, turn_id: str, prompt: str) -> Mapping[str, Any]:
        return {"session_id": session_id, "turn_id": turn_id, "steered": prompt}

    def turn_interrupt(self, session_id: str, turn_id: str) -> Mapping[str, Any]:
        return {"session_id": session_id, "turn_id": turn_id, "interrupted": True}

    def close(self) -> None:
        pass


def request(method: str, params: Mapping[str, Any]) -> dict[str, Any]:
    value = hello_request()
    value["method"] = method
    value["params"] = dict(params)
    return value


def test_lists_and_resumes_sessions_with_approved_cwd() -> None:
    backend = FakeBackend()
    listed = handle_request(
        request("codex.sessions.list", {"project_path": "/srv/project"}), backend
    )
    resumed = handle_request(
        request(
            "codex.session.resume",
            {"session_id": "ses_old", "cwd": "/srv/project", "mode": "inspect"},
        ),
        backend,
    )
    assert listed["result"]["sessions"][0]["session_id"] == "ses_old"
    assert resumed["result"]["mode"] == "inspect"


def test_turn_streams_and_preserves_idempotency_key() -> None:
    events: list[Mapping[str, Any]] = []
    response = handle_request(
        request(
            "codex.turn.start",
            {"session_id": "ses_old", "prompt": "next", "idempotency_key": "watch:42"},
        ),
        FakeBackend(),
        events.append,
    )
    assert response["ok"] is True
    assert response["result"]["status"] == "completed"
    assert events[0]["event"] == "codex.message.delta"


def test_completed_snapshot_recovers_agent_text_after_fast_turn() -> None:
    assert (
        _snapshot_agent_text(
            {
                "items": [
                    {"type": "userMessage", "text": "secret prompt"},
                    {"type": "agentMessage", "text": "first"},
                    {"type": "commandExecution", "aggregatedOutput": "private output"},
                    {"type": "agentMessage", "text": "second"},
                ]
            }
        )
        == "first\n\nsecond"
    )


def test_prompt_limit_and_relative_project_are_rejected() -> None:
    backend = FakeBackend()
    relative = handle_request(request("codex.sessions.list", {"project_path": "relative"}), backend)
    oversized = handle_request(
        request(
            "codex.turn.start",
            {"session_id": "ses", "prompt": "x" * (32 * 1024 + 1), "idempotency_key": "id"},
        ),
        backend,
    )
    assert relative["error"]["code"] == "invalid_request"
    assert oversized["error"]["code"] == "invalid_request"
