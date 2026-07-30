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
plus here-documents inside $( ) whose body holds an odd number of apostrophes
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
        before = lines[index][: match.start()]
        if before.count("$(") - before.count(")") > 0:
            if "\n".join(body).count("'") % 2:
                problems.append("%s:%d: heredoc <<%s inside $( )" % (
                    name, index + 1, delimiter))
        # Skip the body either way. It is data, not shell, and a test fixture
        # that demonstrates this very bug must not be read as an instance of it.
        index = cursor + 1

if problems:
    sys.stderr.write(
        "here-documents inside $( ) with an odd number of apostrophes "
        "(bash 3.2 cannot parse the enclosing file):\n")
    for problem in problems:
        sys.stderr.write("  %s\n" % problem)
    sys.stderr.write(
        "fix: balance the apostrophes, reword the text, or move the script "
        "into its own file\n")
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
