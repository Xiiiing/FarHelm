"""Bounded history pages with resumable UTF-8 message fragments."""

from __future__ import annotations

import base64
import json
from collections.abc import Callable, Mapping, Sequence
from typing import Any

PREFIX = "fh1."
PAGE_BYTES = 500 * 1024  # Leave room for the Agent/Hub envelope below 512 KiB.


def decode_cursor(session: str, cursor: str | None) -> tuple[str | None, dict[str, Any]]:
    if not cursor or not cursor.startswith(PREFIX):
        return cursor, {}
    try:
        raw = cursor[len(PREFIX) :]
        value = json.loads(base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4)))
        if value["session"] != session:
            raise ValueError("history cursor belongs to another session")
        if any(type(value.get(key)) is not int or value[key] < 0 for key in ("t", "i", "offset")):
            raise ValueError("invalid history cursor offset")
        return value.get("upstream"), value
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError("invalid history cursor") from error


def bounded_page(
    session: str,
    turns: Sequence[Mapping[str, Any]],
    upstream: str | None,
    next_cursor: str | None,
    resume: Mapping[str, Any],
    normalise: Callable[[Mapping[str, Any]], Mapping[str, Any]],
) -> Mapping[str, Any]:
    page: dict[str, Any] = {"session_id": session, "turns": [], "next_cursor": next_cursor}
    start_turn = int(resume.get("t", 0))
    if start_turn >= len(turns) and resume:
        raise ValueError("history changed; reload the latest page")
    for ti in range(start_turn, len(turns)):
        turn = dict(normalise(turns[ti]))
        items = turn.pop("items")
        if ti == start_turn and resume and turn["turn_id"] != resume.get("turn"):
            raise ValueError("history changed; reload the latest page")
        output: dict[str, Any] = {**turn, "items": []}
        page["turns"].append(output)
        start_item = int(resume.get("i", 0)) if ti == start_turn else 0
        for ii in range(start_item, len(items)):
            item = dict(items[ii])
            offset = int(resume.get("offset", 0)) if ti == start_turn and ii == start_item else 0
            if (
                resume
                and ti == start_turn
                and ii == start_item
                and item["item_id"] != resume.get("item")
            ):
                raise ValueError("history changed; reload the latest page")
            text = item["text"]
            if not 0 <= offset <= len(text):
                raise ValueError("invalid history offset")
            fragment = {**item, "text": text[offset:], "text_offset": offset, "text_complete": True}
            output["items"].append(fragment)
            if len(json.dumps(page, ensure_ascii=False).encode()) <= PAGE_BYTES:
                continue
            # Binary search accounts for JSON escaping and multi-byte Unicode.
            low, high = 0, len(text) - offset
            while low < high:
                mid = (low + high + 1) // 2
                fragment["text"] = text[offset : offset + mid]
                if len(json.dumps(page, ensure_ascii=False).encode()) <= PAGE_BYTES - 1024:
                    low = mid
                else:
                    high = mid - 1
            fragment["text"] = text[offset : offset + low]
            fragment["text_complete"] = False
            if low == 0:
                output["items"].pop()
                if not output["items"]:
                    page["turns"].pop()
            value = {
                "session": session,
                "upstream": upstream,
                "t": ti,
                "i": ii,
                "offset": offset + low,
                "turn": turn["turn_id"],
                "item": item["item_id"],
            }
            page["next_cursor"] = PREFIX + base64.urlsafe_b64encode(
                json.dumps(value, separators=(",", ":")).encode()
            ).decode().rstrip("=")
            page["continuation"] = {
                "kind": "message" if offset + low > 0 else "history",
                "turn_id": turn["turn_id"],
                "item_id": item["item_id"],
                "text_offset": offset + low,
            }
            return page
    return page
