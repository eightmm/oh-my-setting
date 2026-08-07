#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Arguments are file paths, so a flag has to be answered before the loop treats
# it as one: adding the file-argument form made `--help` try to parse a file
# named --help and exit 127.
case "${1:-}" in
  -h|--help)
    cat <<'EOF'
usage: check-bash32.sh [FILE ...]

Parse every shipped script with a Bash parser and reject bash-4-only syntax,
plus here-documents inside $( ) whose body derails the Bash 3.2 command-
substitution scanner — an open quote or a quote-hidden paren either way
(Bash 3.2 cannot parse the enclosing file). With no arguments the full shipped
set is checked. OMS_BASH32_BIN selects the parser, e.g. /bin/bash on macOS.
EOF
    exit 0
    ;;
  -*)
    echo "error: unknown option: $1" >&2
    exit 2
    ;;
esac

parser="${OMS_BASH32_BIN:-bash}"
command -v "$parser" >/dev/null 2>&1 || {
  echo "error: Bash parser not found: $parser" >&2
  exit 1
}

# Optional file arguments so a regression can point this at a fixture; with
# none, the full shipped set is checked.
if [ "$#" -gt 0 ]; then
  FILES=("$@")
else
  FILES=(install.sh scripts/oms scripts/*.sh scripts/lib/*.sh
    plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh)
fi

for file in "${FILES[@]}"; do
  case "$file" in
    /*) "$parser" -n "$file" ;;
    *) "$parser" -n "$ROOT/$file" ;;
  esac
done

# Bash 3.2 cannot parse a here-document that sits inside $( ) when the body
# holds an odd number of apostrophes — even with a quoted <<'EOF' delimiter,
# which is supposed to stop all processing. Its command-substitution scanner
# still reads the body looking for the closing paren, takes the lone quote as
# the start of a literal, and swallows the ")". The file then fails to parse at
# all, so every caller of the library dies on macOS while Linux is fine.
# Verified against real bash 3.2: a prose apostrophe in a Python heredoc inside
# $( ) is enough, and the same heredoc outside $( ) parses cleanly.
# Nothing but a 3.2 parser catches this, and the only one in CI is the macOS
# leg, so the rule is written down here where every push can check it.
python3 - "${FILES[@]}" <<'SCAN'
import re
import sys

# A herestring (<<<) is not a here-document and must not match.
OPENER = re.compile(r"<<-?[ \t]*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")
problems = []

for name in sys.argv[1:]:
    with open(name, encoding="utf-8", errors="replace") as handle:
        lines = handle.read().splitlines()
    index = 0
    while index < len(lines):
        match = OPENER.search(lines[index])
        if not match:
            index += 1
            continue
        delimiter = match.group(2)
        body = []
        cursor = index + 1
        while cursor < len(lines) and lines[cursor].strip() != delimiter:
            body.append(lines[cursor])
            cursor += 1
        # Unclosed "$(" to the left means this here-document opens inside a
        # command substitution, which is the only place the bug bites.
        # Simulate the 3.2 scanner over the body instead of counting lone
        # apostrophes: an EVEN pair of quotes can still hide a paren between
        # them (that is exactly how gc.sh broke every macOS CI leg while the
        # parity rule stayed green). The body of a data heredoc is safe only
        # when the scan leaves no quote open and no net paren drift.
        before = lines[index][: match.start()]
        if before.count("$(") - before.count(")") > 0:
            text = "\n".join(body)
            state = None
            depth = 0
            k = 0
            while k < len(text):
                c = text[k]
                if state == "'":
                    if c == "'":
                        state = None
                elif state == '"':
                    if c == '"' and text[k - 1] != "\\":
                        state = None
                elif state == "`":
                    if c == "`":
                        state = None
                else:
                    if c == "'":
                        state = "'"
                    elif c == '"':
                        state = '"'
                    elif c == "`":
                        state = "`"
                    elif c == "(":
                        depth += 1
                    elif c == ")":
                        depth -= 1
                k += 1
            if state is not None or depth != 0:
                problems.append(
                    "%s:%d: heredoc <<%s inside $( ) (open quote: %s, paren drift: %+d)" % (
                        name, index + 1, delimiter, state or "none", depth))
        # Skip the body either way. It is data, not shell, and a test fixture
        # that demonstrates this very bug must not be read as an instance of it.
        index = cursor + 1

if problems:
    sys.stderr.write(
        "here-documents inside $( ) whose body derails the bash 3.2 scanner "
        "(the enclosing file fails to parse on macOS):\n")
    for problem in problems:
        sys.stderr.write("  %s\n" % problem)
    sys.stderr.write(
        "fix: reword the body so quotes pair up and no paren hides inside a "
        "quote span, or move the script into its own file\n")
    raise SystemExit(1)
SCAN

hits="$(grep -rnE 'declare[[:space:]]+-A|mapfile|readarray|\$\{[A-Za-z_][A-Za-z0-9_]*(\^\^|,,)' \
  "${FILES[@]}" \
  | grep -vE ':[0-9]+:[[:space:]]*#|check-bash32\.sh:' || true)"
if [ -n "$hits" ]; then
  echo "bash-4-only constructs found (must be bash 3.2 compatible):" >&2
  echo "$hits" >&2
  exit 1
fi

if [ -n "${OMS_BASH32_BIN:-}" ]; then
  echo "bash-3.2: ok ($parser)"
else
  echo "bash-syntax+3.2-static: ok"
fi
