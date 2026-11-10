"""Write what the Results reader shows to a local file. Nothing is uploaded and nothing is added to the text."""

import os
import sys

WIDTH = 100


def write(target, text):
    """Create `target` with the reader's text; 0 on success, 2 when the file cannot be created safely.

    The file is new (an existing path, a symlink included, is refused rather than replaced), private to the
    person (mode 600) and holds exactly the bounded, already-masked text the reader would print: no transcript,
    no patch body and no raw log is pulled in for the export."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(target, flags, 0o600)
    except FileExistsError:
        print("panel: --export refuses to replace %s; name a new file" % target, file=sys.stderr)
        return 2
    except OSError as error:
        print("panel: --export could not create %s (%s)" % (target, error.strerror or "error"), file=sys.stderr)
        return 2
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(text if text.endswith("\n") else text + "\n")
    except OSError as error:
        print("panel: --export could not write %s (%s)" % (target, error.strerror or "error"), file=sys.stderr)
        return 2
    print("panel: wrote %d line(s) to %s" % (text.count("\n") + (not text.endswith("\n")), target))
    return 0
