"""Shared history/stream privacy filtering without damaging Markdown or URLs."""

from __future__ import annotations

import re

# URLs take precedence over filesystem paths. Closing Markdown delimiters are
# outside local paths, so replacing a link destination leaves its syntax intact.
_PATHS = re.compile(
    r"(?P<url>https?://[^\s<>\"`]+|mailto:[^\s<>\"`]+)"
    r"|(?<![\w/])(?:[A-Za-z]:[\\/]|~?/)[^\s\[\]()<>{}'\"`|，。；！？]+"
)
_BOUNDARY = re.compile(r"[\s，。；！？]")


def redact_paths(text: str) -> str:
    if "/" not in text and "\\" not in text:
        return text
    return _PATHS.sub(lambda match: match[0] if match.group("url") else "[local path]", text)


class StreamText:
    """Hold unfinished tokens so a split local path is never partially emitted.

    Offsets count Unicode characters in the sanitized text, like history.py.
    Each upstream item owns one instance; interleaved items never share buffers.
    """

    def __init__(self) -> None:
        self.pending = ""
        self.offset = 0

    def feed(self, delta: str, *, final: bool = False) -> tuple[int, str]:
        self.pending += delta
        end = len(self.pending) if final else 0
        if not final:
            for boundary in _BOUNDARY.finditer(self.pending):
                end = boundary.end()
        text = redact_paths(self.pending[:end])
        self.pending = self.pending[end:]
        offset = self.offset
        self.offset += len(text)
        return offset, text
