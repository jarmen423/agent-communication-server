"""Paging long task results into tool-result-sized pieces.

A page is measured as it will appear in the tool result: JSON-escaped
(``ensure_ascii=False``), so quotes, backslashes and control characters
count at their escaped length and a page never exceeds ``max_chars``. The
cursor is a character offset into the result, which is re-read from
JetStream on every call: no server-side cursor state.
"""

from __future__ import annotations

import json


def _escaped_len(text: str) -> int:
    return len(json.dumps(text, ensure_ascii=False)) - 2


def page_text(text: str, offset: int, max_chars: int) -> dict:
    """``{text, offset, total_chars, next_cursor}`` for the page of ``text``
    starting at ``offset``. ``next_cursor`` is None on the last page."""
    total = len(text)
    offset = max(0, min(offset, total))
    end = min(total, offset + max_chars)
    if _escaped_len(text[offset:end]) > max_chars:
        lo, hi = offset + 1, end  # largest end whose escaped page fits
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if _escaped_len(text[offset:mid]) <= max_chars:
                lo = mid
            else:
                hi = mid - 1
        end = lo
    return {"text": text[offset:end], "offset": offset, "total_chars": total,
            "next_cursor": str(end) if end < total else None}
