"""FarHelm Worker request validation and dispatch."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from farhelm_worker_codex import __version__

WORKER_PROTOCOL = "farhelm-worker/1"
WORKER_NAME = "farhelm-worker-codex"


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
        response["error"] = {
            "code": error_code,
            "message": error_message or "request failed",
        }
    return response


def handle_request(request: Mapping[str, Any]) -> dict[str, Any]:
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
            request_id,
            error_code="invalid_request",
            error_message="kind must be request",
        )
    if not isinstance(request_id_value, str) or not request_id_value:
        return _response(
            "unknown",
            error_code="invalid_request",
            error_message="request_id must be a non-empty string",
        )

    method = request.get("method")
    if method == "worker.hello":
        return _response(
            request_id,
            result={
                "worker": WORKER_NAME,
                "version": __version__,
                "capabilities": ["worker.hello"],
            },
        )
    return _response(
        request_id,
        error_code="method_not_found",
        error_message=f"unsupported method: {method!s}",
    )
