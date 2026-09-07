from collections.abc import Mapping
from types import SimpleNamespace
from typing import Any

from farhelm_worker_codex.display import display_page
from farhelm_worker_codex.text import StreamText, redact_paths
from farhelm_worker_codex.worker import CodexBackend, _normalise_turn


def test_markdown_urls_math_and_paths_have_identical_history_stream_filtering() -> None:
    text = (
        "**训练🙂结果** [文档](https://example.org/a/b?q=1) "
        "[本地](/home/private/result.json) `curl https://example.org/api` "
        "公式 \\(a/b\\) $x/y$ C:\\private\\file.py\n"
    )
    expected = text.replace("/home/private/result.json", "[local path]").replace(
        "C:\\private\\file.py", "[local path]"
    )
    assert redact_paths(text) == expected
    for size in (1, 3, 17, 4096):
        stream = StreamText()
        restored = ""
        for start in range(0, len(text), size):
            offset, delta = stream.feed(text[start : start + size])
            assert offset == len(restored)
            restored += delta
            assert "/home" not in restored and "private" not in restored
        offset, last = stream.feed("", final=True)
        assert offset == len(restored)
        assert restored + last == expected
    turn = _normalise_turn(
        {"id": "t", "items": [{"id": "i", "type": "agentMessage", "text": text}]}
    )
    assert turn["items"][0]["text"] == expected


def test_index_search_reaches_unloaded_rows_and_scopes_bindings_without_history() -> None:
    threads = [
        SimpleNamespace(
            id=f"s-{i:04}", name=None, preview=f"训练结果 {i}", updated_at=1000 - i, cwd="/approved"
        )
        for i in range(1000)
    ]
    bindings = {t.id: {"project_id": "p", "cwd": "/approved"} for t in threads}
    params = {"mode": "search", "agent_id": "a", "bindings": bindings, "query": "结果 998"}
    result = display_page(((t, False) for t in threads), params)
    assert [r["session_id"] for r in result["sessions"]] == ["s-0998"]
    assert result["sessions"][0]["display_label"] == "训练结果 998"
    del params["query"]
    first = display_page(((t, False) for t in threads), params)
    assert first["has_more"]
    assert len(first["sessions"]) == 51
    params["after"] = {"agent_id": "a", "session_id": "s-0049", "updated_at_unix": 951}
    second = display_page(((t, False) for t in threads), params)
    assert second["sessions"][0]["session_id"] == "s-0050"
    assert display_page(((t, True) for t in threads), params)["sessions"] == []
    threads[50].cwd = "/not-approved"
    assert display_page(((threads[50], False),), params)["sessions"] == []


def test_display_labels_are_single_line_and_placeholder_names_use_preview() -> None:
    thread = SimpleNamespace(
        id="s",
        name="Codex session",
        preview="  第一条\n用户消息🙂  " * 50,
        updated_at=1,
        cwd="/approved",
    )
    params = {
        "mode": "labels",
        "query": None,
        "bindings": {"s": {"project_id": "p", "cwd": "/approved"}},
    }
    label = display_page([(thread, False)], params)["sessions"][0]["display_label"]
    assert len(label) == 80 and "\n" not in label
    thread.name = "正式名称"
    assert display_page([(thread, False)], params)["sessions"][0]["display_label"] == "正式名称"


def test_interleaved_items_preserve_ids_offsets_and_flush_before_terminal() -> None:
    class Client:
        def __init__(self) -> None:
            self.events = iter(
                [
                    ("item/agentMessage/delta", {"itemId": "a", "delta": "第一条 /home/"}),
                    ("item/agentMessage/delta", {"itemId": "b", "delta": "第二条🙂 "}),
                    ("item/agentMessage/delta", {"itemId": "a", "delta": "private/result.py 完成"}),
                    ("turn/completed", {"turn": {"status": "completed"}}),
                ]
            )

        def turn_start(self, *_args: Any) -> SimpleNamespace:
            return SimpleNamespace(turn=SimpleNamespace(id="t"))

        def next_turn_notification(self, _turn: str) -> SimpleNamespace:
            method, payload = next(self.events)
            return SimpleNamespace(method=method, payload=payload)

    backend: Any = CodexBackend.__new__(CodexBackend)
    backend._client = Client()
    events: list[Mapping[str, Any]] = []
    backend.turn_start("s", "prompt", "operation", events.append)
    assert events[0]["event"] == "codex.turn.started"
    assert events[-1]["event"] == "codex.turn.completed"
    deltas = [e["data"] for e in events if e["event"] == "codex.message.delta"]
    assert [(d["item_id"], d["text_offset"], d["delta"]) for d in deltas] == [
        ("a", 0, "第一条 [local path] 完成"),
        ("b", 0, "第二条🙂 "),
    ]


def test_new_empty_session_is_persisted_without_a_synthetic_prompt_or_public_title() -> None:
    calls: list[tuple[str, str]] = []

    class Client:
        def thread_start(self, params: Mapping[str, Any]) -> SimpleNamespace:
            assert params["sandbox"] == "read-only"
            return SimpleNamespace(
                thread=SimpleNamespace(id="new-empty", name=None, cwd="/approved", updated_at=1)
            )

        def thread_set_name(self, session: str, name: str) -> None:
            calls.append((session, name))

    backend: Any = CodexBackend.__new__(CodexBackend)
    backend._client = Client()
    result = backend.session_start("/approved", "inspect")
    assert calls == [("new-empty", "Codex session")]
    assert result["session_id"] == "new-empty" and result["title"] is None
