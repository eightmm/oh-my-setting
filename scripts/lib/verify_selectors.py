"""Static OMS suite-selector validation shared by planning and dispatch."""
import ast
import difflib
import os
from pathlib import Path
import re
import shlex


def operator_sections(root):
    """Section names from tests/operator/runner.py SECTIONS, parsed as text
    (plan time must not execute the target tree); None when unreadable."""
    try:
        with open(os.path.join(root, "tests", "operator", "runner.py"), encoding="utf-8") as handle:
            tree = ast.parse(handle.read())
        for node in tree.body:
            if (isinstance(node, ast.Assign) and len(node.targets) == 1
                    and isinstance(node.targets[0], ast.Name) and node.targets[0].id == "SECTIONS"):
                return [(name, filename) for name, _, filename in ast.literal_eval(node.value)]
    except (OSError, SyntaxError, ValueError, TypeError):
        pass
    return None


def operator_section_cases(root, section, sections):
    """Case names of a section, in order; None when unreadable. Mirrors
    tests/operator/runner.py body() and case_markers() `# BEGIN X` handling."""
    try:
        filename = dict(sections)[section]
        if filename:
            path = os.path.join(root, "tests", "operator", filename)
        else:
            path = os.path.join(root, "tests", "operator-tools-smoke.sh")
        with open(path, encoding="utf-8") as handle:
            code = handle.read()
    except (OSError, ValueError, TypeError):
        return None
    if not filename:
        begin, end = "# OPERATOR_SECTION_BEGIN dispatch_exit\n", "# OPERATOR_SECTION_END dispatch_exit\n"
        if begin not in code:
            return None
        code = code.split(begin, 1)[1].split(end, 1)[0]
    cases = []
    for line in code.splitlines():
        match = re.match(r"# (BEGIN|END) (.+)$", line)
        if match and match.group(1) == "BEGIN":
            cases.append(re.split(r" \(|\.", match.group(2), 1)[0].strip())
    return cases


def _read_suite(path):
    try:
        if path.is_symlink() or not path.is_file():
            raise ValueError("--verify suite must be a regular file: %s" % path.name)
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError("could not read --verify suite %s" % path.name) from error


def _uses_smoke_runner(suite, target):
    if suite == "tests/run-smoke-shard.sh":
        return True
    try:
        return "run-smoke-shard.sh" in target.read_text(encoding="utf-8")
    except OSError:
        return False


def _selector_suggestions(name, valid):
    if not valid:
        return []
    components = [value.rsplit("/", 1)[-1] for value in valid]
    close_components = difflib.get_close_matches(name, components, n=len(components))
    candidates = [value for component in close_components for value in valid
                  if value.rsplit("/", 1)[-1] == component]
    candidates += difflib.get_close_matches(name, valid, n=len(valid))
    return list(dict.fromkeys(candidates))[:3]


def validate_verify_selectors(repo, verify, continuation_patch=None, patch_repo=None):
    """Refuse missing suite files and unknown selectors without running repository code.

    A continuation may also select smoke tests defined on added hunk lines of
    its digest-verified carried patch, only in the corresponding suite.
    """
    if not isinstance(verify, str) or not verify.strip():
        return
    try:
        lexer = shlex.shlex(verify, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        words = list(lexer)
    except ValueError:
        return
    added = {}
    if continuation_patch:
        from panel_results import _patch_name, verified_patch
        body = verified_patch(patch_repo or repo, {"patch": continuation_patch.get("patch"),
                                                  "patch_sha256": continuation_patch.get("sha256")})
        current, remaining = None, 0
        for line in body.decode("utf-8", "surrogateescape").split("\n"):
            if line.startswith("diff --git "):
                current, remaining = None, 0
            elif line.startswith("@@ "):
                hunk = re.match(r"@@ -[0-9]+(?:,([0-9]+))? \+[0-9]+(?:,([0-9]+))? @@", line)
                remaining = sum(int(count) if count is not None else 1 for count in hunk.groups()) if hunk else 0
            elif remaining:
                definition = re.match(r"\+(test_[A-Za-z0-9_]+)\(\) *\{", line)
                if current and definition:
                    added.setdefault(current, []).append(definition.group(1))
                remaining = max(0, remaining - (2 if line.startswith(" ") else 1
                                                if line.startswith(("+", "-")) else 0))
            elif line.startswith("+++ "):
                current = _patch_name(line[4:].split("\t", 1)[0])
    # Native runners (pytest, npm, make, cargo, go) are explicitly unvalidated.
    separators = {"&&", "||", ";", "|", "&", ";;", "(", ")"}
    repo = Path(repo)
    for index, word in enumerate(words[:-1]):
        if word != "bash":
            continue
        suite = words[index + 1]
        if not re.fullmatch(r"(?:tests|scripts)/[A-Za-z0-9_.-]+\.sh", suite):
            continue
        target = repo / suite
        if not target.is_file():
            raise ValueError("--verify names missing suite file: %s" % suite)
        cursor = index + 2
        end = next((position for position in range(cursor, len(words))
                    if words[position] in separators), len(words))
        sections = [words[position + 1] for position in range(cursor, end - 1)
                    if words[position] == "--section"]
        seen_sections = False
        while cursor < len(words):
            option = words[cursor]
            if option in separators:
                break  # selectors after a shell separator belong to the next command
            if option not in {"--section", "--case", "--only"}:
                cursor += 1
                continue
            if option == "--section" and suite == "tests/operator-tools-smoke.sh":
                if seen_sections:
                    raise ValueError("--verify repeats --section; use one comma-separated list")
                seen_sections = True
            if cursor + 1 >= len(words):
                break
            selector = words[cursor + 1]
            # Selectors are read from the suite text, never by running repository code (dry runs included).
            if option == "--section" and suite == "tests/operator-tools-smoke.sh":
                source = _read_suite(target)
                valid = [name.replace("_", "-") for name in
                         re.findall(r"^# OPERATOR_SECTION_BEGIN ([a-z_-]+)$", source, re.M)
                         if name not in {"core", "shared_cache"}]
                # Split-out sections are registered in the runner manifest.
                valid = list(dict.fromkeys(valid + re.findall(
                    r'^    \("([a-z/-]+)", [0-9]+, ', _read_suite(repo / "tests/operator/runner.py"), re.M)))
            elif option == "--case" and suite == "tests/operator-tools-smoke.sh":
                manifest = operator_sections(repo)
                if not manifest or len(sections) != 1 or sections[0] not in dict(manifest):
                    cursor += 2
                    continue
                valid = operator_section_cases(repo, sections[0], manifest)
                if valid is None:
                    cursor += 2
                    continue
            elif option == "--only" and _uses_smoke_runner(suite, target):
                selected_suite = (repo / "tests/scripts-smoke.sh" if suite == "tests/run-smoke-shard.sh"
                                  else target)
                valid = re.findall(r"^(test_[A-Za-z0-9_]+)\(\) *\{", _read_suite(selected_suite), re.M)
                valid += added.get(selected_suite.relative_to(repo).as_posix(), [])
            else:
                cursor += 2
                continue
            # run-smoke-shard.sh matches one --only argument by literal spaces; a tab, newline or
            # comma would select nothing and exit 0, so those stay part of an (unknown) name.
            if option == "--case":
                requested = [selector]
            elif option == "--section":
                requested = selector.split(",")
            elif selector.startswith("test_"):
                requested = [name for name in selector.split(" ") if name]
            else:
                requested = [selector]
            unknown = [name for name in requested if name not in valid]
            if unknown:
                hints = []
                for name in unknown:
                    matches = _selector_suggestions(name, valid)
                    if matches:
                        label = "%s: " % name if len(unknown) > 1 else ""
                        hints.append("%sdid you mean: %s" % (label, ", ".join(matches)))
                hint = "; " + "; ".join(hints) if hints else ""
                raise ValueError("unknown %s value(s): %s%s; valid names: %s" %
                                 (option, ", ".join(unknown), hint, ", ".join(valid)))
            cursor += 2
