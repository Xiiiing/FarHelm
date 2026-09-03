"""Executable framed-stdio Worker loop."""

from __future__ import annotations

import logging
import sys
from typing import BinaryIO

from farhelm_worker_codex.framing import FrameError, read_frame, write_frame
from farhelm_worker_codex.worker import handle_request

LOGGER = logging.getLogger("farhelm-worker-codex")


def run(stdin: BinaryIO, stdout: BinaryIO) -> int:
    """Serve requests until Agent closes stdin."""

    while True:
        request = read_frame(stdin)
        if request is None:
            return 0
        write_frame(stdout, handle_request(request))


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
