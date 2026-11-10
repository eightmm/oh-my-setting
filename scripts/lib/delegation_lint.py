#!/usr/bin/env python3
"""Advisory text lint for a delegation brief and for the result a worker returns.

    delegation_lint.py brief FILE|- [--strict]
    delegation_lint.py result FILE|- [--writer]
    delegation_lint.py review FILE|- --verdict=pass|fail

Mechanical regex checks only: no model call, no network, nothing written. Human
warnings go to stderr as `warning: brief: ...` / `warning: result: ...`; stdout
carries the comma-separated warning codes (empty when none) so a caller can
record them. Exit 0 always, except `brief --strict` which exits 1 on any
warning. A clean lint says the text has the expected shape, nothing more: it is
never verification evidence.

`review` lints one gate seat's final answer and adds only what the gate does
not already check: the GATE line, SECURITY line, answer quality and the fenced
`findings` block's own validation (review_findings.py) stay where they are.
"""

import re
import sys

# A brief this small is its own goal; the structural checks would only nag.
SHORT_BRIEF_BYTES = 240
# More numbered/step lines than this, with no stated goal, is a script.
STEP_LIMIT = 7
SUMMARY_CAP_BYTES = 1200
REVIEW_CAP_BYTES = 6000
MAX_BYTES = 262144

WARNING_TEXT = {
    "no-goal": 'no stated goal (add a "Goal:" line saying what the work is for)',
    "no-scope": 'no scope or out-of-scope statement (add "Scope:" / "Out of scope:")',
    "no-output-form": 'no expected output form (add "Output:" naming what to return)',
    "no-stop-condition": 'no stop condition (add "Done when:" / "Stop if:")',
    "no-evidence-pointer": "no file path pointers (name the files or path:line the work starts from)",
    "over-prescribed": "step-by-step script with no stated goal; state the goal and let the worker choose the steps",
    "no-summary": "no summary paragraph",
    "summary-too-long": "summary exceeds the %d-byte cap; lead with a short summary" % SUMMARY_CAP_BYTES,
    "finding-without-pointer": "findings without a path:line pointer",
    "no-unresolved-line": 'no explicit unresolved line (write "Unresolved: none" when nothing is open)',
    "no-changed-files": "writer result lists no changed files",
    "findings-not-trackable": "prose findings but no fenced findings block, so nothing is tracked",
    "pass-with-blocking": "GATE: pass contradicts a blocking finding in the findings block",
    "long-answer-no-summary": "answer over %d bytes with no summary line" % REVIEW_CAP_BYTES,
}

LABEL = r"^[\s>#*_`\-+\d.)(]*"
GOAL = re.compile(LABEL + r"(goals?|objectives?|purpose|목표|목적)\b", re.I)
SCOPE = re.compile(LABEL + r"((out[\s-]of[\s-])?scope|non[\s-]goals?|constraints?|범위|비범위|제외|제약)\b", re.I)
OUTPUT = re.compile(LABEL + r"((expected |required )?(outputs?|deliverables?|results?|report)|output form|산출물|출력|보고|결과)\b"
                    r"|" + LABEL + r"(return|report back|reply with|respond with|write up)\b", re.I)
STOP = re.compile(r"\b(done when|definition of done|stop(ping)? (when|if|condition)|stop condition|finished when|"
                  r"acceptance criteria|success criteria|exit criteria)\b|완료 조건|종료 조건|중단 조건|완료 기준", re.I)
PATH = re.compile(r"(?<![\w/.-])(?:[\w.-]+/)+[\w.-]+|(?<![\w/.-])[\w-]+\.(?:py|sh|md|json|jsonl|toml|ya?ml|txt|"
                  r"ts|js|c|h|go|rs|cfg|ini)\b")
POINTER = re.compile(r"[\w./-]+\.\w+:\d+")
STEP = re.compile(r"^\s*(?:\d+[.)]|step\s*\d+\b)", re.I)


def is_label(line, pattern):
    stripped = line.strip()
    if not stripped:
        return False
    if re.match(r"^#{1,6}\s", stripped):
        return bool(pattern.match(stripped))
    return bool(pattern.match(stripped)) and (":" in stripped[:40] or len(stripped) <= 40)


def lint_brief(text):
    if len(text.encode("utf-8", "replace")) < SHORT_BRIEF_BYTES:
        return []
    lines = text.splitlines()
    codes = []
    has_goal = any(is_label(line, GOAL) for line in lines)
    if not has_goal:
        codes.append("no-goal")
    if not any(is_label(line, SCOPE) for line in lines):
        codes.append("no-scope")
    if not any(is_label(line, OUTPUT) for line in lines):
        codes.append("no-output-form")
    if not STOP.search(text):
        codes.append("no-stop-condition")
    if not PATH.search(text):
        codes.append("no-evidence-pointer")
    if not has_goal and sum(1 for line in lines if STEP.match(line)) > STEP_LIMIT:
        codes.append("over-prescribed")
    return codes


SUMMARY_HEADING = re.compile(r"^[\s#*_`]*(summary|what changed|result|tl;?dr|요약|결과)\b[\s*_`]*:?\s*(.*)$", re.I)
FINDINGS_HEADING = re.compile(r"^[\s#*_`]*(findings?|issues?|must[\s-]fix|problems?|발견|지적)\b", re.I)
HEADING = re.compile(r"^\s*(#{1,6}\s|\*\*[^*]+\*\*:?\s*$)")
UNRESOLVED = re.compile(r"^[\s>#*_`\-+]*(unresolved|remaining uncertainty|open (questions?|items?)|blockers?|"
                        r"not verified|미해결|남은 (불확실|문제))\b", re.I)
CHANGED = re.compile(r"^[\s>#*_`\-+]*(changed files?|files? changed|files? (touched|modified)|what changed|변경 (파일|내용))\b",
                     re.I)


def first_paragraph(lines):
    para = []
    for line in lines:
        if line.strip():
            para.append(line)
        elif para:
            break
    return para


def section_after(lines, index):
    """Lines under the heading at `index`, up to the next heading."""
    body = []
    for line in lines[index + 1:]:
        if HEADING.match(line):
            break
        body.append(line)
    return body


def lint_result(text, writer=False):
    lines = text.splitlines()
    codes = []
    summary = []
    for number, line in enumerate(lines):
        match = SUMMARY_HEADING.match(line)
        if match:
            summary = [match.group(2)] if match.group(2).strip() else []
            summary = summary or first_paragraph(section_after(lines, number))
            break
    if not summary:
        summary = first_paragraph([line for line in lines if not HEADING.match(line)])
    summary_text = "\n".join(summary).strip()
    if not summary_text:
        codes.append("no-summary")
    elif len(summary_text.encode("utf-8", "replace")) > SUMMARY_CAP_BYTES:
        codes.append("summary-too-long")

    for number, line in enumerate(lines):
        if not FINDINGS_HEADING.match(line):
            continue
        items = [item for item in section_after(lines, number) if re.match(r"^\s*(?:[-*+]|\d+[.)])\s+\S", item)]
        if any(not POINTER.search(item) for item in items):
            codes.append("finding-without-pointer")
        break

    if not any(UNRESOLVED.match(line) for line in lines):
        codes.append("no-unresolved-line")

    if writer:
        listed = False
        for number, line in enumerate(lines):
            if CHANGED.match(line):
                listed = bool(PATH.search(line) or PATH.search("\n".join(section_after(lines, number))))
                if listed:
                    break
        if not listed:
            codes.append("no-changed-files")
    return codes


NONE_ITEM = re.compile(r"^\W*(none|no (blocking |actionable )?(findings?|issues?)|n/?a|nothing|없음)\b", re.I)
LABEL_LINE = re.compile(r"^[\s#*_`]*[A-Z][\w ]{1,30}:\s*\**\s*$")
ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+(\S.*)$")
RECOMMENDATION = re.compile(r"^[\s#*_`]*(recommendation|verdict|결론)\b", re.I)


def prose_findings(lines):
    """Bullet items under the findings heading up to the next label; none-items dropped."""
    for number, line in enumerate(lines):
        if not FINDINGS_HEADING.match(line):
            continue
        items = []
        for item in lines[number + 1:]:
            if HEADING.match(item) or LABEL_LINE.match(item) or item.lstrip().startswith("```"):
                break
            match = ITEM.match(item)
            if match and not NONE_ITEM.match(match.group(1)):
                items.append(match.group(1))
        return items
    return []


def lint_review(text, verdict):
    """Shape warnings for a gate seat's final answer that carries a GATE verdict."""
    from review_findings import parse_blocks
    lines = text.splitlines()
    codes = []
    found, _rejected = parse_blocks(text)
    has_block = any(re.match(r"^\s*```+\s*findings\s*$", line) for line in lines)
    items = prose_findings(lines)
    if items and not has_block:
        codes.append("findings-not-trackable")
        if any(not POINTER.search(item) for item in items):
            codes.append("finding-without-pointer")
    if verdict == "pass" and any(item["severity"] == "blocking" for item in found):
        codes.append("pass-with-blocking")
    if len(text.encode("utf-8", "replace")) > REVIEW_CAP_BYTES and not any(
            SUMMARY_HEADING.match(line) or RECOMMENDATION.match(line) for line in lines):
        codes.append("long-answer-no-summary")
    return codes


def main(argv):
    args = [arg for arg in argv if not arg.startswith("--")]
    flags = {arg for arg in argv if arg.startswith("--")}
    verdicts = [arg[10:] for arg in flags if arg.startswith("--verdict=")]
    flags -= {"--verdict=" + value for value in verdicts}
    if len(args) != 2 or args[0] not in ("brief", "result", "review") or flags - {"--strict", "--writer"} or \
            (args[0] == "review") != (len(verdicts) == 1) or any(value not in ("pass", "fail") for value in verdicts):
        sys.stderr.write(__doc__)
        return 2
    kind, source = args
    try:
        raw = sys.stdin.buffer.read(MAX_BYTES) if source == "-" else open(source, "rb").read(MAX_BYTES)
    except OSError as exc:
        sys.stderr.write("error: %s\n" % exc)
        return 2
    text = raw.decode("utf-8", "replace")
    if kind == "review":
        codes = lint_review(text, verdicts[0])
    else:
        codes = lint_brief(text) if kind == "brief" else lint_result(text, writer="--writer" in flags)
    for code in codes:
        sys.stderr.write("warning: %s: %s\n" % (kind, WARNING_TEXT[code]))
    sys.stdout.write(",".join(codes) + "\n")
    return 1 if codes and kind == "brief" and "--strict" in flags else 0


if __name__ == "__main__":
    sys.dont_write_bytecode = True
    sys.exit(main(sys.argv[1:]))
