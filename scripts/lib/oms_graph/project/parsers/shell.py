"""Shell parser (W2)."""

from __future__ import annotations

import re
import warnings

from ..model import make_edge, make_node, node_id
from .base import ParseResult, Parser
from .python import PythonParser

# A script invoked by path, whether bare (`bash scripts/x.sh`), quoted, or
# behind a directory variable (`"$ROOT/scripts/x.sh"`); the variable prefix
# marks the reference INFERRED because the prefix is resolved by convention.
INVOCATION_RE = re.compile(r"(\$\{?[A-Za-z_][A-Za-z0-9_]*\}?/)?((?:[A-Za-z0-9_][A-Za-z0-9_.-]*/)+[A-Za-z0-9_][A-Za-z0-9_.-]*\.(?:sh|bash|py))\b")
COMMAND_SUB_RE = re.compile(r"\$\(\s*(?:[A-Za-z_][A-Za-z0-9_]*=[^\s()]+\s+)*(?:!\s*)?([A-Za-z_][A-Za-z0-9_]*)")
STATIC_CASE_ARM_RE = re.compile(r"^\s*([A-Za-z0-9_./*?@:+-]+(?:\|[A-Za-z0-9_./*?@:+-]+)*)\)(.*)$")
CLEAR_CASE_OPEN_RE = re.compile(r"^\s*case\s+(\"(?:\\.|[^\"])*\"|'[^']*'|[^\s]+)\s+in\s*(?:#.*)?$")
SHELL_CONTROL_RE = re.compile(r"\b(?:case|esac)\b|;;&|;&|;;")
ARM_TERMINATOR_RE = re.compile(r"(?:;;&|;&|;;)\s*$")
PYTHON_STDIN_RE = re.compile(r"^\s*(?:[A-Za-z_][A-Za-z0-9_]*=[^\s]+\s+)*(?:python|python3(?:\.\d+)?)\s+(?:-\s|(?=<<))")
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


def _shell_code(raw: str):
    """Return unquoted, uncommented shell code and whether quotes contain control syntax."""
    code = []
    quote = ""
    quoted = []
    escaped = False
    comment = False
    for index, char in enumerate(raw):
        if comment:
            break
        if quote:
            if escaped:
                quoted.append(" ")
                escaped = False
            elif char == "\\" and quote != "'":
                quoted.append(" ")
                escaped = True
            elif char == quote:
                quote = ""
                code.append(" ")
            else:
                quoted.append(char)
            continue
        if escaped:
            code.append(" ")
            escaped = False
            continue
        if char in ("'", '"', "`"):
            quote = char
            code.append(" ")
            continue
        if char == "\\":
            code.append(" ")
            escaped = True
            continue
        if char == "#" and (index == 0 or raw[index - 1].isspace()):
            comment = True
            continue
        code.append(char)
    quoted_control = bool(SHELL_CONTROL_RE.search("".join(quoted)))
    return "".join(code), quoted_control


def _case_open_count(code: str) -> int:
    """Count case openers in command positions, including nested arm bodies."""
    count = 0
    for match in re.finditer(r"\bcase\b", code):
        prefix = code[:match.start()].rstrip()
        command_position = (not prefix or prefix.endswith((";", "&", "|", "(", ")", "{"))
                            or re.search(r"\b(?:then|do|else)\s*$", prefix))
        if command_position and re.search(r"\bin\b", code[match.end():]):
            count += 1
    return count


def _case_close_count(code: str) -> int:
    """Count unquoted esac words that appear in command positions."""
    return len(re.findall(r"(?:^|[;(){}])\s*esac\b", code))


class ShellParser(Parser):
    language = "shell"
    extensions = (".sh", ".bash")
    version = 5

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        result = ParseResult()
        file_id = node_id("file", path)
        if path.startswith("scripts/lib/"):
            module_id = node_id("module", path)
            result.nodes.append(make_node("module", path.rsplit("/", 1)[-1], path, self.language, source_digest))
            result.edges.append(make_edge(file_id, module_id, "contains", "EXTRACTED", path=path, source_digest=source_digest))
        heredoc = ""
        heredoc_python = False
        heredoc_strip_tabs = False
        heredoc_body = []
        heredoc_line = 0
        case_depth = 0
        case_patterns = False
        case_arm_head = False
        current_function = ""
        function_nodes = {}
        pending_comments = []

        def add_ref(ref):
            result.refs.append(ref)
            if current_function:
                scoped = dict(ref)
                scoped["from"] = node_id("function", path, current_function)
                result.refs.append(scoped)

        for number, raw in enumerate((line.rstrip("\r") for line in text.split("\n")), 1):
            if heredoc:
                delimiter_line = raw.lstrip("\t") if heredoc_strip_tabs else raw
                if delimiter_line.strip() == heredoc:
                    heredoc = ""
                    if heredoc_python:
                        with warnings.catch_warnings():
                            warnings.simplefilter("ignore")
                            parsed = PythonParser().parse(path, "\n".join(heredoc_body), source_digest)
                        for ref in parsed.refs:
                            if ref.get("relation") == "imports" and ref.get("from") == file_id:
                                imported = dict(ref)
                                imported["line"] += heredoc_line
                                imported["heredoc_python"] = True
                                add_ref(imported)
                    heredoc_python = False
                    heredoc_strip_tabs = False
                    heredoc_body = []
                elif heredoc_python:
                    heredoc_body.append(
                        "" if len(raw) > 4096 else (raw.lstrip("\t") if heredoc_strip_tabs else raw)
                    )
                continue
            if len(raw) > 4096:
                pending_comments = []
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
                heredoc_python = bool(PYTHON_STDIN_RE.match(raw))
                heredoc_strip_tabs = bool(re.search(r"<<-", raw))
                heredoc_body = []
                heredoc_line = number
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
            case_arm = None
            code, quoted_control = _shell_code(raw)
            opener = CLEAR_CASE_OPEN_RE.match(raw)
            if opener and any(token in opener.group(1) for token in ("$(", "`", "\\")):
                opener = None
            opened_case = False
            if case_depth == 0 and opener and not quoted_control and not raw.rstrip().endswith("\\"):
                case_depth = 1
                case_patterns = True
                case_arm_head = True
                opened_case = True
            if case_depth == 1 and case_patterns and not opened_case:
                control_words = bool(re.search(r"\b(?:case|esac)\b", code))
                terminators = re.findall(r";;&|;&|;;", code)
                if quoted_control or raw.rstrip().endswith("\\") or control_words:
                    case_patterns = False
                elif case_arm_head:
                    arm = STATIC_CASE_ARM_RE.match(raw)
                    if arm and not any(char in arm.group(1) for char in "'\"$`\\"):
                        case_arm = arm.group(2)
                        case_arm_head = False
                    elif not re.match(r"\s*esac\s*(?:#.*)?$", raw):
                        case_patterns = False
                if terminators:
                    if ARM_TERMINATOR_RE.search(code) and len(terminators) == 1 and not quoted_control:
                        if case_patterns:
                            case_arm_head = True
                    else:
                        case_patterns = False
            elif case_depth > 0 and (quoted_control or raw.rstrip().endswith("\\")):
                case_patterns = False
            scan_raw = case_arm if case_arm is not None else raw
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
            for match in INVOCATION_RE.finditer(scan_raw):
                cleaned = match.group(2).lstrip("./")
                if "/" in cleaned and cleaned != path:
                    add_ref({"from": file_id, "relation": "calls", "kind": "path", "value": cleaned, "line": number, "variable": bool(match.group(1))})
            for command in _command_names(scan_raw):
                if command not in CONTROL_WORDS and command not in WRAPPER_WORDS:
                    add_ref({"from": file_id, "relation": "calls", "kind": "name", "value": command, "line": number, "shell_bare": True})
            if not opened_case:
                case_depth = max(0, case_depth + _case_open_count(code) - _case_close_count(code))
            if case_depth == 0:
                case_patterns = False
                case_arm_head = False
        return result
