from __future__ import annotations

import io
import struct

import pytest

from farhelm_worker_codex.framing import MAX_FRAME_BYTES, FrameError, read_frame, write_frame


def test_frame_round_trip_preserves_unicode() -> None:
    stream = io.BytesIO()
    value = {"message": "训练完成", "sequence": 3}
    write_frame(stream, value)
    stream.seek(0)
    assert read_frame(stream) == value


def test_clean_eof_returns_none() -> None:
    assert read_frame(io.BytesIO()) is None


def test_truncated_payload_is_rejected() -> None:
    with pytest.raises(FrameError, match="truncated"):
        read_frame(io.BytesIO(struct.pack(">I", 8) + b"{}"))


def test_oversized_length_is_rejected_before_payload_read() -> None:
    with pytest.raises(FrameError, match="exceeds maximum"):
        read_frame(io.BytesIO(struct.pack(">I", MAX_FRAME_BYTES + 1)))
