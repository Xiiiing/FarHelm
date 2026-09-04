"""Validated FarHelm-to-Codex lifecycle adapter."""

from __future__ import annotations

from collections.abc import Callable, Mapping
from pathlib import Path
from time import monotonic
from typing import Any, Protocol, cast

from farhelm_worker_codex import __version__

WORKER_PROTOCOL = "farhelm-worker/1"
WORKER_NAME = "farhelm-worker-codex"
PROMPT_LIMIT = 32 * 1024
CAPABILITIES = [
    "worker.hello",
    "codex.sessions.list",
    "codex.session.start",
    "codex.session.resume",
    "codex.turn.start",
    "codex.turn.steer",
    "codex.turn.interrupt",
]
Emit = Callable[[Mapping[str, Any]], None]


class Backend(Protocol):
    def sessions_list(self, project_path: str) -> Mapping[str, Any]: ...
    def session_start(self, cwd: str, mode: str) -> Mapping[str, Any]: ...
    def session_resume(self, session_id: str, cwd: str, mode: str) -> Mapping[str, Any]: ...
    def turn_start(
        self, session_id: str, prompt: str, idempotency_key: str, emit: Emit
    ) -> Mapping[str, Any]: ...
    def turn_steer(self, session_id: str, turn_id: str, prompt: str) -> Mapping[str, Any]: ...
    def turn_interrupt(self, session_id: str, turn_id: str) -> Mapping[str, Any]: ...
    def close(self) -> None: ...


class CodexBackend:
    """Small compatibility layer around the pinned official Python SDK."""

    def __init__(self) -> None:
        from openai_codex.client import CodexClient

        self._client = CodexClient()
        self._client.start()
        self._client.initialize()

    def close(self) -> None:
        self._client.close()

    def sessions_list(self, project_path: str) -> Mapping[str, Any]:
        response = self._client.thread_list({"cwd": project_path, "archived": False, "limit": 100})
        sessions = []
        for thread in response.data:
            if Path(str(thread.cwd)) != Path(project_path):
                continue
            sessions.append(
                {
                    "session_id": thread.id,
                    "title": thread.name or thread.preview,
                    "cwd": str(thread.cwd),
                    "created_at_unix": thread.created_at,
                    "updated_at_unix": thread.updated_at,
                }
            )
        return {"sessions": sessions, "next_cursor": response.next_cursor}

    def session_start(self, cwd: str, mode: str) -> Mapping[str, Any]:
        response = self._client.thread_start(
            {"cwd": cwd, "sandbox": _sandbox(mode), "approvalPolicy": "on-request"}
        )
        return _thread_result(response.thread)

    def session_resume(self, session_id: str, cwd: str, mode: str) -> Mapping[str, Any]:
        response = self._client.thread_resume(
            session_id,
            {"cwd": cwd, "sandbox": _sandbox(mode), "approvalPolicy": "on-request"},
        )
        if Path(str(response.thread.cwd)) != Path(cwd):
            raise ValueError("session cwd does not match approved project")
        return _thread_result(response.thread)

    def turn_start(
        self, session_id: str, prompt: str, idempotency_key: str, emit: Emit
    ) -> Mapping[str, Any]:
        started = self._client.turn_start(
            session_id, prompt, {"clientUserMessageId": idempotency_key}
        )
        turn_id = started.turn.id
        emit(_event("codex.turn.started", {"session_id": session_id, "turn_id": turn_id}))
        snapshot = self._client.thread_read(session_id, include_turns=True).thread
        for turn in snapshot.turns:
            if turn.id != turn_id:
                continue
            turn_data = _model_json(turn)
            status = str(turn_data.get("status", "inProgress"))
            if status != "inProgress":
                recovered_delta = _snapshot_agent_text(turn_data)
                if recovered_delta:
                    emit(
                        _event(
                            "codex.message.delta",
                            {
                                "session_id": session_id,
                                "turn_id": turn_id,
                                "delta": recovered_delta,
                            },
                        )
                    )
                emit(_event("codex.turn.completed", {"turn": turn_data}))
                return {"session_id": session_id, "turn_id": turn_id, "status": status}
            break
        delta_buffer = ""
        last_flush = monotonic()
        while True:
            notification = self._client.next_turn_notification(turn_id)
            data = _model_json(notification.payload)
            if notification.method == "item/agentMessage/delta":
                delta = data.get("delta")
                if isinstance(delta, str):
                    delta_buffer += delta
                if len(delta_buffer.encode("utf-8")) >= 4096 or monotonic() - last_flush >= 0.2:
                    emit(
                        _event(
                            "codex.message.delta",
                            {"session_id": session_id, "turn_id": turn_id, "delta": delta_buffer},
                        )
                    )
                    delta_buffer = ""
                    last_flush = monotonic()
            elif notification.method == "turn/completed":
                if delta_buffer:
                    emit(
                        _event(
                            "codex.message.delta",
                            {"session_id": session_id, "turn_id": turn_id, "delta": delta_buffer},
                        )
                    )
                emit(_event("codex.turn.completed", data))
                completed_turn = data.get("turn")
                status = (
                    completed_turn.get("status", "completed")
                    if isinstance(completed_turn, dict)
                    else "completed"
                )
                return {"session_id": session_id, "turn_id": turn_id, "status": status}

    def turn_steer(self, session_id: str, turn_id: str, prompt: str) -> Mapping[str, Any]:
        return _model_json(self._client.turn_steer(session_id, turn_id, prompt))

    def turn_interrupt(self, session_id: str, turn_id: str) -> Mapping[str, Any]:
        return _model_json(self._client.turn_interrupt(session_id, turn_id))


def _sandbox(mode: str) -> str:
    if mode == "inspect":
        return "read-only"
    if mode == "edit":
        return "workspace-write"
    raise ValueError("mode must be inspect or edit")


def _snapshot_agent_text(turn: Mapping[str, Any]) -> str:
    items = turn.get("items")
    if not isinstance(items, list):
        return ""
    messages = []
    for item in items:
        if not isinstance(item, dict) or item.get("type") != "agentMessage":
            continue
        value = item.get("text")
        if isinstance(value, str) and value:
            messages.append(value)
    return "\n\n".join(messages)


def _thread_result(thread: Any) -> Mapping[str, Any]:
    return {
        "session_id": str(thread.id),
        "cwd": str(thread.cwd),
        "title": thread.name or thread.preview,
        "updated_at_unix": int(thread.updated_at),
    }


def _model_json(value: Any) -> dict[str, Any]:
    if hasattr(value, "model_dump"):
        return cast(dict[str, Any], value.model_dump(mode="json", by_alias=True))
    if isinstance(value, dict):
        return cast(dict[str, Any], value)
    return {"value": str(value)}


def _event(name: str, data: Mapping[str, Any]) -> Mapping[str, Any]:
    return {"protocol": WORKER_PROTOCOL, "kind": "event", "event": name, "data": dict(data)}


def _response(
    request_id: str,
    *,
    result: Mapping[str, Any] | None = None,
    error_code: str | None = None,
    error_message: str | None = None,
) -> dict[str, Any]:
    response: dict[str, Any] = {
        "protocol": WORKER_PROTOCOL,
        "kind": "response",
        "request_id": request_id,
        "ok": error_code is None,
    }
    if error_code is None:
        response["result"] = dict(result or {})
    else:
        response["error"] = {"code": error_code, "message": error_message or "request failed"}
    return response


def handle_request(
    request: Mapping[str, Any], backend: Backend | None = None, emit: Emit | None = None
) -> dict[str, Any]:
    """Validate and handle one versioned Worker request."""

    request_id_value = request.get("request_id")
    request_id = request_id_value if isinstance(request_id_value, str) else "unknown"
    if request.get("protocol") != WORKER_PROTOCOL:
        return _response(
            request_id,
            error_code="unsupported_protocol",
            error_message=f"expected {WORKER_PROTOCOL}",
        )
    if request.get("kind") != "request":
        return _response(
            request_id, error_code="invalid_request", error_message="kind must be request"
        )
    if not isinstance(request_id_value, str) or not request_id_value:
        return _response(
            "unknown", error_code="invalid_request", error_message="request_id must be non-empty"
        )
    method = request.get("method")
    if method == "worker.hello":
        return _response(
            request_id,
            result={"worker": WORKER_NAME, "version": __version__, "capabilities": CAPABILITIES},
        )
    if method not in CAPABILITIES:
        return _response(
            request_id,
            error_code="method_not_found",
            error_message=f"unsupported method: {method}",
        )
    params = request.get("params")
    if not isinstance(params, Mapping):
        return _response(
            request_id, error_code="invalid_request", error_message="params must be an object"
        )
    owned_backend = backend is None
    try:
        service = backend or CodexBackend()
        output = _dispatch(str(method), params, service, emit or (lambda _event: None))
        return _response(request_id, result=output)
    except (KeyError, TypeError, ValueError) as error:
        return _response(request_id, error_code="invalid_request", error_message=str(error))
    except Exception as error:  # SDK errors are normalized at this private boundary.
        return _response(request_id, error_code="codex_error", error_message=str(error)[:512])
    finally:
        if owned_backend and "service" in locals():
            service.close()


def _dispatch(
    method: str, params: Mapping[str, Any], backend: Backend, emit: Emit
) -> Mapping[str, Any]:
    if method == "codex.sessions.list":
        return backend.sessions_list(_absolute(params, "project_path"))
    if method == "codex.session.start":
        return backend.session_start(_absolute(params, "cwd"), _string(params, "mode"))
    if method == "codex.session.resume":
        return backend.session_resume(
            _string(params, "session_id"), _absolute(params, "cwd"), _string(params, "mode")
        )
    if method == "codex.turn.start":
        return backend.turn_start(
            _string(params, "session_id"),
            _prompt(params),
            _string(params, "idempotency_key"),
            emit,
        )
    if method == "codex.turn.steer":
        return backend.turn_steer(
            _string(params, "session_id"), _string(params, "turn_id"), _prompt(params)
        )
    if method == "codex.turn.interrupt":
        return backend.turn_interrupt(_string(params, "session_id"), _string(params, "turn_id"))
    raise ValueError(f"unsupported method: {method}")


def _string(params: Mapping[str, Any], name: str) -> str:
    value = params.get(name)
    if not isinstance(value, str) or not value:
        raise ValueError(f"{name} must be a non-empty string")
    return value


def _absolute(params: Mapping[str, Any], name: str) -> str:
    value = _string(params, name)
    if not Path(value).is_absolute():
        raise ValueError(f"{name} must be absolute")
    return value


def _prompt(params: Mapping[str, Any]) -> str:
    prompt = _string(params, "prompt")
    if len(prompt.encode("utf-8")) > PROMPT_LIMIT:
        raise ValueError("prompt exceeds 32 KiB")
    return prompt
