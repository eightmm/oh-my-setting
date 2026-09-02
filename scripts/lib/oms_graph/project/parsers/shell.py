"""Shell parser (W2)."""

from __future__ import annotations

import re

from ..model import make_edge, make_node, node_id
from .base import ParseResult, Parser

# A script invoked by path, whether bare (`bash scripts/x.sh`), quoted, or
# behind a directory variable (`"$ROOT/scripts/x.sh"`); the variable prefix
# marks the reference INFERRED because the prefix is resolved by convention.
INVOCATION_RE = re.compile(r"(\$\{?[A-Za-z_][A-Za-z0-9_]*\}?/)?((?:[A-Za-z0-9_][A-Za-z0-9_.-]*/)+[A-Za-z0-9_][A-Za-z0-9_.-]*\.(?:sh|bash|py))\b")
COMMAND_SUB_RE = re.compile(r"\$\(\s*(?:[A-Za-z_][A-Za-z0-9_]*=[^\s()]+\s+)*(?:!\s*)?([A-Za-z_][A-Za-z0-9_]*)")
CONTROL_WORDS = {
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do",
    "done", "case", "esac", "select", "function", "source", "return",
    "local", "export", "readonly", "declare", "typeset", "in",
}
WRAPPER_WORDS = {"command", "builtin", "env", "time", "nohup", "sudo"}


def _command_names(raw: str):
    """Bounded likely command positions in common compound shell syntax."""
    names = [match.group(1) for match in COMMAND_SUB_RE.finditer(raw)]
    for segment in re.split(r"&&|\|\||[;|]", raw):
        tokens = segment.strip().split()
        while tokens and (tokens[0] in CONTROL_WORDS or tokens[0] in ("!", "{", "(")):
            tokens.pop(0)
        while tokens and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", tokens[0]):
            tokens.pop(0)
        if tokens and tokens[0] in WRAPPER_WORDS:
            tokens.pop(0)
            while tokens and (tokens[0].startswith("-") or re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", tokens[0])):
                tokens.pop(0)
        if tokens and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", tokens[0]):
            names.append(tokens[0])
    return names


class ShellParser(Parser):
    language = "shell"
    extensions = (".sh", ".bash")
    version = 3

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        result = ParseResult()
        file_id = node_id("file", path)
        if path.startswith("scripts/lib/"):
            module_id = node_id("module", path)
            result.nodes.append(make_node("module", path.rsplit("/", 1)[-1], path, self.language, source_digest))
            result.edges.append(make_edge(file_id, module_id, "contains", "EXTRACTED", path=path, source_digest=source_digest))
        heredoc = ""
        current_function = ""
        function_nodes = {}
        pending_comments = []

        def add_ref(ref):
            result.refs.append(ref)
            if current_function:
                scoped = dict(ref)
                scoped["from"] = node_id("function", path, current_function)
                result.refs.append(scoped)

        for number, raw in enumerate(text.splitlines(), 1):
            if len(raw) > 4096:
                pending_comments = []
                continue
            if heredoc:
                if raw.strip() == heredoc:
                    heredoc = ""
                continue
            stripped = raw.strip()
            if stripped.startswith("#"):
                if not stripped.startswith("#!"):
                    pending_comments.append(stripped.lstrip("#").strip())
                continue
            if not stripped:
                pending_comments = []
                continue
            here = re.search(r"<<-?\s*['\"]?([A-Za-z_][A-Za-z0-9_]*)", raw)
            if here:
                heredoc = here.group(1)
            if current_function and re.match(r"^}\s*(?:#.*)?$", raw):
                function_nodes[current_function]["metadata"]["end_line"] = number
                current_function = ""
                pending_comments = []
                continue
            match = re.match(r"\s*(?:function\s+([A-Za-z_][A-Za-z0-9_]*)|([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\))", raw)
            if match:
                name = match.group(1) or match.group(2)
                ident = node_id("function", path, name)
                multiline = raw == raw.lstrip() and re.search(r"\{\s*(?:#.*)?$", raw)
                metadata = {"signature": name + "()", "end_line": number}
                function_node = make_node(
                    "function", name, path, self.language, source_digest,
                    qualname=name, line=number, summary=" ".join(pending_comments),
                    metadata=metadata,
                )
                result.nodes.append(function_node)
                function_nodes[name] = function_node
                result.edges.append(make_edge(file_id, ident, "contains", "EXTRACTED", path=path, source_digest=source_digest, line=number))
                # Function-level refs are precision only: every ref below also
                # keeps its file source. Scope only the canonical multi-line
                # form so an inline body cannot absorb the following file.
                current_function = name if multiline else ""
                pending_comments = []
                continue
            pending_comments = []
            include = re.match(r"\s*(?:source|\.)\s+(.+?)\s*$", raw)
            if include:
                argument = include.group(1).strip()
                quoted = re.match(r"^(['\"])(.*)\1(?:\s.*)?$", argument)
                target = (quoted.group(2) if quoted else argument.split()[0]).strip("'\"")
                variable = "$" in target or "$(dirname" in target
                cleaned = re.sub(r"^(?:\$\{?(?:ROOT|SCRIPTS_DIR)\}?/?|\$\(dirname\s+['\"]?\$0['\"]?\)/?)", "", target)
                cleaned = cleaned.replace("scripts/lib/../", "scripts/").lstrip("./")
                if cleaned:
                    add_ref({"from": file_id, "relation": "imports", "kind": "path", "value": cleaned, "line": number, "variable": variable, "shell_include": True})
                continue
            for match in INVOCATION_RE.finditer(raw):
                cleaned = match.group(2).lstrip("./")
                if "/" in cleaned and cleaned != path:
                    add_ref({"from": file_id, "relation": "calls", "kind": "path", "value": cleaned, "line": number, "variable": bool(match.group(1))})
            for command in _command_names(raw):
                if command not in CONTROL_WORDS and command not in WRAPPER_WORDS:
                    add_ref({"from": file_id, "relation": "calls", "kind": "name", "value": command, "line": number, "shell_bare": True})
        return result
