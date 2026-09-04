"""Executable framed-stdio Worker loop."""

from __future__ import annotations

import logging
import sys
import threading
from collections.abc import Mapping
from typing import Any, BinaryIO

from farhelm_worker_codex.framing import FrameError, read_frame, write_frame
from farhelm_worker_codex.worker import Backend, CodexBackend, handle_request

LOGGER = logging.getLogger("farhelm-worker-codex")


def run(stdin: BinaryIO, stdout: BinaryIO, backend: Backend | None = None) -> int:
    """Serve requests until Agent closes stdin."""

    service = backend
    owned_backend = False
    output_lock = threading.Lock()
    turn_threads: list[threading.Thread] = []

    def emit(value: Mapping[str, Any]) -> None:
        with output_lock:
            write_frame(stdout, value)

    def handle(request: dict[str, object]) -> None:
        assert service is not None
        response = handle_request(request, service, emit)
        emit(response)

    try:
        while True:
            request = read_frame(stdin)
            if request is None:
                for thread in turn_threads:
                    thread.join()
                return 0
            if request.get("method") != "worker.hello" and service is None:
                service = CodexBackend()
                owned_backend = True
            if request.get("method") == "codex.turn.start":
                thread = threading.Thread(target=handle, args=(request,), name="codex-turn")
                thread.start()
                turn_threads.append(thread)
            else:
                emit(handle_request(request, service, emit))
    finally:
        if owned_backend and service is not None:
            service.close()


def main() -> None:
    logging.basicConfig(stream=sys.stderr, level=logging.INFO)
    try:
        exit_code = run(sys.stdin.buffer, sys.stdout.buffer)
    except (FrameError, OSError) as error:
        LOGGER.error("worker protocol failure: %s", error)
        exit_code = 2
    raise SystemExit(exit_code)


if __name__ == "__main__":
    main()
