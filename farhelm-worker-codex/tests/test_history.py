import json

import pytest

from farhelm_worker_codex.history import bounded_page, decode_cursor
from farhelm_worker_codex.worker import _normalise_turn


def test_large_unicode_message_resumes_without_truncation_or_duplicate_text():
    text = "训练🙂结果\n" * 100_000
    turns = [
        {
            "id": "turn",
            "status": "completed",
            "items": [{"id": "item", "type": "agentMessage", "text": text}],
        }
    ]
    cursor = None
    restored = ""
    pages = 0
    while True:
        upstream, resume = decode_cursor("session", cursor)
        page = bounded_page("session", turns, upstream, None, resume, _normalise_turn)
        assert len(json.dumps(page, ensure_ascii=False).encode()) < 512 * 1024
        for turn in page["turns"]:
            for item in turn["items"]:
                assert item["text_offset"] == len(restored)
                restored += item["text"]
        pages += 1
        cursor = page["next_cursor"]
        if cursor is None:
            break
        with pytest.raises(ValueError):
            decode_cursor("other-session", cursor)
        assert pages < 20
    assert pages > 1
    assert restored == text


def test_cursor_is_bound_to_turn_and_item_even_at_offset_zero():
    resume = {"t": 0, "i": 0, "offset": 0, "turn": "turn", "item": "different"}
    turns = [{"id": "turn", "items": [{"id": "item", "type": "agentMessage", "text": "hello"}]}]
    with pytest.raises(ValueError):
        bounded_page("s", turns, None, None, resume, _normalise_turn)


def test_full_messages_redact_paths_without_truncating_prose():
    text = "训练🙂" * 1000 + " /private/project/file.py complete"
    turn = _normalise_turn(
        {"id": "t", "items": [{"id": "i", "type": "agentMessage", "text": text}]}
    )
    assert turn["items"][0]["text"] == "训练🙂" * 1000 + " [local path] complete"
