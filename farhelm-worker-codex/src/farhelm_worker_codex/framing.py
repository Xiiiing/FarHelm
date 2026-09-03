"""Length-prefixed JSON framing for the private Agent-to-Worker channel."""

from __future__ import annotations

import json
import struct
from collections.abc import Mapping
from typing import Any, BinaryIO

MAX_FRAME_BYTES = 8 * 1024 * 1024


class FrameError(ValueError):
    """Raised when a Worker frame is truncated, oversized, or invalid."""


def _read_exact(stream: BinaryIO, length: int, *, allow_clean_eof: bool = False) -> bytes | None:
    chunks: list[bytes] = []
    remaining = length
    while remaining:
        chunk = stream.read(remaining)
        if not chunk:
            if allow_clean_eof and not chunks:
                return None
            raise FrameError(f"truncated frame: expected {length} bytes")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_frame(stream: BinaryIO) -> dict[str, Any] | None:
    """Read one frame, returning None only for EOF before a new frame."""

    header = _read_exact(stream, 4, allow_clean_eof=True)
    if header is None:
        return None
    length = struct.unpack(">I", header)[0]
    if length > MAX_FRAME_BYTES:
        raise FrameError(f"frame length {length} exceeds maximum {MAX_FRAME_BYTES}")
    payload = _read_exact(stream, length)
    assert payload is not None
    try:
        value: Any = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FrameError("frame is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise FrameError("frame JSON must be an object")
    return value


def write_frame(stream: BinaryIO, value: Mapping[str, Any]) -> None:
    """Write and flush one compact JSON frame."""

    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(payload) > MAX_FRAME_BYTES:
        raise FrameError(f"frame length {len(payload)} exceeds maximum {MAX_FRAME_BYTES}")
    stream.write(struct.pack(">I", len(payload)))
    stream.write(payload)
    stream.flush()
