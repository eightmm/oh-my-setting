#!/usr/bin/env python3
"""Mechanical contradiction check for a memory note that is about to be appended.

Read-only and advisory: it prints one line per older active note that appears
to contradict the new text and always exits 0, so the append still succeeds and
nothing extra is recorded. No model is called. Only three narrow shapes count:

  key       the same `KEY: value` subject with a disjoint set of values
  polarity  `use X` against `do not use X` / `never X` / `avoid X`, same subject
            and the same trailing qualifier ("in prod", "for tests", ...)

Both notes must carry the same path scope (or none); a note cited to a source
file counts as a different scope per file.

Prose, questions, conditionals, fenced blocks and quotes are skipped, so the
check misses most contradictions on purpose. The new note's text is never
echoed back (it may still be refused by the sensitive-content scan).
"""
import argparse
import importlib.util
import os
import re
import sys
import unicodedata

LIB = os.path.dirname(os.path.abspath(__file__))
COMPARED_KINDS = ("note", "imported", "pin")
MAX_NOTES = 500
MAX_LINES = 40
MAX_LINE = 300
MAX_WARNINGS = 3
GENERIC_KEYS = frozenset(
    "note notes why because context decision decisions summary todo fix example examples see "
    "warning tip result results task goal next next step imported from source link url ref refs "
    "reason rationale evidence impact background problem issue cause symptom lesson details detail "
    "rule update status state progress outcome files verification".split()
)
NEG = r"(?:do not|don't|dont|never|must not|mustn't|should not|shouldn't|cannot|can't|avoid|no longer|stop using)"
POS = r"(?:use|using|prefer|run)"
SUBJECT_PREFIX = r"(?:(?:we|you|agents?|i|they|all)\s+)?"
NEG_RE = re.compile(r"^(?:(?:and|but|so|also|please)\s+)*%s(?:always\s+)?%s\s+(?P<rest>.+)$" % (SUBJECT_PREFIX, NEG))
POS_RE = re.compile(r"^(?:(?:and|but|so|also|please)\s+)*%s(?:always\s+|must\s+|should\s+)?%s\s+(?P<rest>.+)$" % (SUBJECT_PREFIX, POS))
VERBS = frozenset(("use", "using", "run", "call", "invoke"))
QUALIFIERS = frozenset(
    "for in on when if unless because with from to instead over at during while as since until "
    "before after via here there now than".split()
)
STOP = frozenset("the a an our this that these those any your my it them".split())
KV_RE = re.compile(r"^\s*(?:[-*+]\s+|\d+[.)]\s+)?(?:\*\*)?([^:=\n]{1,40}?)(?:\*\*)?\s*[:=]\s*(.+?)\s*$")
CONDITIONAL_RE = re.compile(r"^(?:if|when|unless|once|while|after|before|until|whenever)\b")


def load_index():
    spec = importlib.util.spec_from_file_location("oms_agent_memory_db", os.path.join(LIB, "agent-memory-db.py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module  # dataclasses resolve the module by name
    spec.loader.exec_module(module)
    return module


def norm(text):
    text = unicodedata.normalize("NFKC", text).lower().replace("’", "'")
    text = re.sub(r"[`\"*_]", "", text)
    return re.sub(r"\s+", " ", text).strip(" \t.;,!")


def body_lines(body):
    fenced = False
    for line in body.splitlines()[:MAX_LINES]:
        stripped = line.strip()
        if stripped.startswith("```") or stripped.startswith("~~~"):
            fenced = not fenced
            continue
        if fenced or not stripped or stripped.startswith(">") or len(stripped) > MAX_LINE or stripped.endswith("?"):
            continue
        yield line


def subject_of(rest):
    """(subject, qualifier) of the object phrase after a polarity word."""
    ticked = re.match(r"^`([^`]{1,80})`\s*(.*)$", rest)
    if ticked:
        return norm(ticked.group(1)), norm(ticked.group(2))
    words = norm(rest).split()
    while words and words[0] in VERBS:
        words.pop(0)
    while words and words[0] in STOP:
        words.pop(0)
    subject = []
    for index, word in enumerate(words):
        if word in QUALIFIERS or len(subject) == 4:
            return " ".join(subject), " ".join(words[index:])
        subject.append(word)
    return " ".join(subject), ""


def statements(body):
    """({key: {values}}, {(subject, qualifier): {polarity}}) for one note body."""
    pairs, polarity = {}, {}
    for line in body_lines(body):
        found = KV_RE.match(line)
        if found:
            key, value = norm(found.group(1)), norm(found.group(2))
            if (re.match(r"[a-z]", key) and len(key.split()) <= 4 and key not in GENERIC_KEYS
                    and "/" not in key and value and len(value) <= 120 and not value.startswith("//")):
                pairs.setdefault(key, set()).add(value)
                continue
        plain = norm(line)
        if CONDITIONAL_RE.match(plain):
            continue
        for clause in re.split(r"(?:[;,]|\.(?=\s|$))\s*", unicodedata.normalize("NFKC", line).lower().replace("’", "'")):
            clause = clause.strip(" \t-*+")
            for pattern, sign in ((NEG_RE, "neg"), (POS_RE, "pos")):
                found = pattern.match(clause)
                if found:
                    subject, qualifier = subject_of(found.group("rest"))
                    if len(subject) >= 2 and subject not in STOP:
                        polarity.setdefault((subject, qualifier), set()).add(sign)
                    break
    return pairs, polarity


def conflicts(new, old):
    """Reasons the older statements contradict the new ones (old values only)."""
    reasons = []
    for key, values in new[0].items():
        before = old[0].get(key)
        if before and not (values & before):
            reasons.append('key "%s" was "%s"' % (key, sorted(before)[0][:60]))
    for ident, signs in new[1].items():
        before = old[1].get(ident)
        if before and len(signs) == 1 and len(before) == 1 and signs != before:
            reasons.append('"%s" was %s' % (ident[0], "forbidden" if "neg" in before else "advised"))
    return reasons


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--shared", required=True)
    parser.add_argument("--pins", required=True)
    parser.add_argument("--note-file", required=True)
    parser.add_argument("--paths", default="")
    parser.add_argument("--source-file", default="")
    args = parser.parse_args()
    with open(args.note_file, encoding="utf-8", errors="replace") as handle:
        new = statements(handle.read())
    if not (new[0] or new[1]) or not os.path.isfile(args.shared):
        return 0
    index = load_index()
    notes, pinned, status = index.memory_state(args.shared, args.pins)
    scope = (args.paths, args.source_file)
    lines = {}
    for source, entries, parser_fn in (("shared.md", notes, index.parse_shared), ("pins.md", pinned, index.parse_pins)):
        spans = []
        try:
            with open(args.shared if source == "shared.md" else args.pins, encoding="utf-8", errors="replace") as handle:
                parser_fn(handle.read(), spans)
        except OSError:
            spans = []
        if len(spans) == len(entries):
            for entry, span in zip(entries, spans):
                lines[entry.event_id] = (source, span[0] + 1)
    found = []
    for entry in (notes + pinned)[-MAX_NOTES:]:
        if entry.event_id in status or entry.kind not in COMPARED_KINDS or not entry.body.strip():
            continue
        if (entry.paths, entry.source_path) != scope:
            continue
        reasons = conflicts(new, statements(entry.body))
        if reasons:
            where = lines.get(entry.event_id)
            place = "%s line %d" % where if where else "no line"
            found.append((entry.event_id, entry.occurred_at, place, reasons[0], len(reasons)))
    for event_id, occurred_at, place, reason, count in reversed(found[-MAX_WARNINGS:]):
        print("memory: warning: this note may contradict %s (%s, %s): %s%s" % (
            event_id, occurred_at, place, reason, " (+%d more)" % (count - 1) if count > 1 else ""))
        print("memory: if it replaces that note, run: oms agent-memory supersede --id %s --text TEXT" % event_id)
    if len(found) > MAX_WARNINGS:
        print("memory: warning: %d more older notes may contradict it" % (len(found) - MAX_WARNINGS))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:  # advisory only: never turn an append into a failure
        raise SystemExit(0)
