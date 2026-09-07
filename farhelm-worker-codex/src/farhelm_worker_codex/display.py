"""Transient labels/search over Codex's thread index, never transcript reads."""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from typing import Any

from .text import redact_paths

PLACEHOLDERS = {"codex session", "untitled", "new conversation", "new chat", "未命名会话", "新会话"}


def formal_title(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    value = " ".join(value.split())
    return value if value and value.casefold() not in PLACEHOLDERS else None


def display_page(
    threads: Iterable[tuple[Any, bool]], params: Mapping[str, Any]
) -> Mapping[str, Any]:
    bindings = params.get("bindings", {})
    agent = str(params.get("agent_id", ""))
    query = str(params.get("query") or "").strip().casefold()
    archive = params.get("archived", "false")
    mode = params.get("mode")
    if mode not in {"labels", "search"} or archive not in {"false", "true", "all"}:
        raise ValueError("invalid display request")
    after = params.get("after")
    after_key = (
        (-int(after["updated_at_unix"]), str(after["agent_id"]), str(after["session_id"]))
        if isinstance(after, Mapping)
        else None
    )
    rows: dict[str, dict[str, Any]] = {}
    for thread, archived in threads:
        binding = bindings.get(thread.id)
        if not isinstance(binding, Mapping):
            continue
        # Exact approved binding is provided locally by Agent, never by Hub.
        cwd = getattr(thread.cwd, "root", thread.cwd)
        if str(cwd) != binding.get("cwd"):
            continue
        if archive != "all" and archived != (archive == "true"):
            continue
        title = formal_title(thread.name)
        preview = str(getattr(thread, "preview", "") or "")
        if query and query not in (title or "").casefold() and query not in preview.casefold():
            continue
        updated = int(thread.updated_at)
        if after_key and (-updated, agent, thread.id) <= after_key:
            continue
        label = " ".join(redact_paths(preview).split())[:80]
        rows[thread.id] = {
            "session_id": thread.id,
            "project_id": binding["project_id"],
            "display_label": title or label or None,
            "updated_at_unix": updated,
        }
    ordered = sorted(rows.values(), key=lambda row: (-row["updated_at_unix"], row["session_id"]))
    return {"sessions": ordered[:51], "has_more": len(ordered) > 50}
