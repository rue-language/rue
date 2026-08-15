"""The one Rust comment/literal masker every source-scanning gate uses.

Three gates used to carry their own copy, and the copies disagreed about
Rust lifetimes: one advanced two characters past the quote and misread
``'a`` in ``<'a, T>``, one required the closing quote by escape-aware
stepping, and one required it by regex on the same line. This module is the
union of those behaviors with the disagreement resolved:

- a quote is a character literal only when a closing quote follows on the
  same line (``CHAR_LITERAL`` below), which is also what keeps a lifetime —
  ``'a``, ``'static``, ``'_`` — unmasked;
- multi-character escapes (``'\\u{1F600}'``) and byte literals (``b'x'``)
  are character literals too, which the escape-stepping copies missed;
- raw and byte strings (``r".."``, ``br#".."#``), nested block comments,
  and ordinary strings mask exactly as before.

Masking preserves byte offsets and newlines: every masked byte becomes a
space, so line/column arithmetic on the masked text matches the original.
"""

from __future__ import annotations

import re
from typing import List, Tuple

RAW_STRING_START = re.compile(r'(?:br|r)(?P<hashes>#{0,255})"')

# A character literal is a quote around exactly one unit — a single
# character or a single escape (`\n`, `\x41`, `\u{1F600}`) — with the
# closing quote immediately after. Anything looser re-creates the bug this
# module exists to fix: with two lifetimes on one line (`<'a, 'static>`),
# a "closing quote somewhere on the line" rule reads `'a, '` as a literal
# and masks real code between the lifetimes.
CHAR_LITERAL = re.compile(
    r"b?'(?:\\(?:u\{[0-9a-fA-F_]{1,6}\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'"
)


def mask_rust_non_code(source: str) -> Tuple[str, List[Tuple[int, int]]]:
    """Blank comments and literal contents, preserving offsets and newlines.

    Returns the masked text and the ``(start, end)`` spans of the comments,
    for gates whose exceptions live in comments (``# ...-ok:`` annotations).
    """
    masked = list(source)
    comments: List[Tuple[int, int]] = []

    def hide(start: int, end: int) -> None:
        for index in range(start, min(end, len(masked))):
            if masked[index] != "\n":
                masked[index] = " "

    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end < 0 else end
            comments.append((index, end))
            hide(index, end)
            index = end
            continue

        if source.startswith("/*", index):
            start = index
            depth = 1
            index += 2
            while index < len(source) and depth:
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            comments.append((start, index))
            hide(start, index)
            continue

        raw = RAW_STRING_START.match(source, index)
        if raw:
            terminator = '"' + raw.group("hashes")
            end = source.find(terminator, raw.end())
            end = len(source) if end < 0 else end + len(terminator)
            hide(index, end)
            index = end
            continue

        quote = index + 1 if source.startswith('b"', index) else index
        if quote < len(source) and source[quote] == '"':
            end = quote + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            hide(index, end)
            index = max(end, index + 1)
            continue

        literal = CHAR_LITERAL.match(source, index)
        if literal and source[index] in "b'":
            hide(literal.start(), literal.end())
            index = literal.end()
            continue

        index += 1

    return "".join(masked), comments
