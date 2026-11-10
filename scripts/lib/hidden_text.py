#!/usr/bin/env python3
"""Mark invisible Unicode in untrusted text (stdin -> stdout).

Tag characters, bidirectional controls and zero-width/format characters render
as nothing, so text that carries them can hide instructions from a human
reader. Each one is replaced by a visible <U+XXXX> marker and one summary line
is printed to stderr. This is a marker, not a defence: it makes no claim of
catching every hiding technique. Ordinary text, including newline, tab and
non-ASCII letters, is left byte-identical.
"""

import sys

RANGES = (
    (0x00AD, 0x00AD),    # soft hyphen
    (0x061C, 0x061C),    # Arabic letter mark
    (0x180E, 0x180E),    # Mongolian vowel separator
    (0x200B, 0x200F),    # zero-width space/joiners, LRM, RLM
    (0x202A, 0x202E),    # bidi embedding and override
    (0x2060, 0x2064),    # word joiner, invisible operators
    (0x2066, 0x206F),    # bidi isolates, deprecated format controls
    (0xFE00, 0xFE0F),    # variation selectors
    (0xFEFF, 0xFEFF),    # zero-width no-break space / BOM
    (0xE0000, 0xE007F),  # tag characters
    (0xE0100, 0xE01EF),  # variation selectors supplement
)


def is_hidden(char):
    code = ord(char)
    return any(low <= code <= high for low, high in RANGES)


def mark(text):
    """Return (marked_text, count)."""
    count = 0
    out = []
    for char in text:
        if is_hidden(char):
            count += 1
            out.append("<U+%04X>" % ord(char))
        else:
            out.append(char)
    return "".join(out), count


def main():
    raw = sys.stdin.buffer.read()
    text = raw.decode("utf-8", errors="surrogateescape")
    marked, count = mark(text)
    sys.stdout.buffer.write(marked.encode("utf-8", errors="surrogateescape"))
    sys.stdout.buffer.flush()
    if count:
        sys.stderr.write("hidden-text: %d invisible character(s) marked as <U+XXXX>\n" % count)
    return 0


if __name__ == "__main__":
    sys.exit(main())
