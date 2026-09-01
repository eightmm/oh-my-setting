"""Shell parser (W2)."""

from __future__ import annotations

import re

from ..model import make_edge, make_node, node_id
from .base import ParseResult, Parser

# A script invoked by path, whether bare (`bash scripts/x.sh`), quoted, or
# behind a directory variable (`"$ROOT/scripts/x.sh"`); the variable prefix
# marks the reference INFERRED because the prefix is resolved by convention.
INVOCATION_RE = re.compile(r"(\$\{?[A-Za-z_][A-Za-z0-9_]*\}?/)?((?:[A-Za-z0-9_][A-Za-z0-9_.-]*/)+[A-Za-z0-9_][A-Za-z0-9_.-]*\.(?:sh|bash|py))\b")


class ShellParser(Parser):
    language = "shell"
    extensions = (".sh", ".bash")
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        result = ParseResult()
        file_id = node_id("file", path)
        if path.startswith("scripts/lib/"):
            module_id = node_id("module", path)
            result.nodes.append(make_node("module", path.rsplit("/", 1)[-1], path, self.language, source_digest))
            result.edges.append(make_edge(file_id, module_id, "contains", "EXTRACTED", path=path, source_digest=source_digest))
        definitions = []
        heredoc = ""
        for number, raw in enumerate(text.splitlines(), 1):
            if len(raw) > 4096:
                continue
            if heredoc:
                if raw.strip() == heredoc:
                    heredoc = ""
                continue
            here = re.search(r"<<-?\s*['\"]?([A-Za-z_][A-Za-z0-9_]*)", raw)
            if here:
                heredoc = here.group(1)
            match = re.match(r"\s*(?:function\s+([A-Za-z_][A-Za-z0-9_]*)|([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\))", raw)
            if match:
                name = match.group(1) or match.group(2)
                ident = node_id("function", path, name)
                definitions.append(name)
                result.nodes.append(make_node("function", name, path, self.language, source_digest, qualname=name, line=number))
                result.edges.append(make_edge(file_id, ident, "contains", "EXTRACTED", path=path, source_digest=source_digest, line=number))
                continue
            include = re.match(r"\s*(?:source|\.)\s+(.+?)\s*$", raw)
            if include:
                argument = include.group(1).strip()
                quoted = re.match(r"^(['\"])(.*)\1(?:\s.*)?$", argument)
                token = (quoted.group(2) if quoted else argument.split()[0]).strip("'\"")
                variable = "$" in token or "$(dirname" in token
                cleaned = re.sub(r"^(?:\$\{?(?:ROOT|SCRIPTS_DIR)\}?/?|\$\(dirname\s+['\"]?\$0['\"]?\)/?)", "", token)
                cleaned = cleaned.replace("scripts/lib/../", "scripts/").lstrip("./")
                if cleaned:
                    result.refs.append({"from": file_id, "relation": "imports", "kind": "path", "value": cleaned, "line": number, "variable": variable, "shell_include": True})
                continue
            for match in INVOCATION_RE.finditer(raw):
                cleaned = match.group(2).lstrip("./")
                if "/" in cleaned and cleaned != path:
                    result.refs.append({"from": file_id, "relation": "calls", "kind": "path", "value": cleaned, "line": number, "variable": bool(match.group(1))})
            command = raw.strip().split()
            if command and re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", command[0]) and command[0] not in ("if", "then", "fi", "for", "while", "do", "done", "case", "esac", "function", "source", "return", "local", "export") and command[0] not in definitions:
                result.refs.append({"from": file_id, "relation": "calls", "kind": "name", "value": command[0], "line": number, "shell_bare": True})
        return result
