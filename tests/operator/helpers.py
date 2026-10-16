"""Shared deterministic inputs and input/geometry helpers; no predecessor tests run."""
def crowd(report, advisors, workers, label=None, parent="a2", prefix="x"):
    crowded = deepcopy(report)
    for role, count, model in (("advisor", advisors, "gpt-6-astra"), ("worker", workers, "gpt-6-luna")):
        for n in range(count):
            ident = "%s-%s-%d" % (prefix, role, n)
            crowded["room"]["participants"].append({"participant": ident, "role": role, "joined": True, "model": model,
                                                    "parent": parent, "label": ident})
            crowded["attempts"]["active_recent"].append({"attempt_id": ident, "state": "working", "panel": {
                "room_id": "r", "room_participant": ident, "role": role}})
    if label:
        for member in crowded["room"]["participants"]:
            if member["role"] in ("advisor", "worker"):
                member["model"], member["label"] = "claude-opus-5-5", label
    return crowded
def closed_cards(text, width, unicode=True):
    opened = False
    for line in text.splitlines():
        if unicode and line.startswith("╭"):
            assert not opened and line.endswith("╮") and display_width(line) == width, ("bad card top", line, opened, width)
            opened = True
        elif unicode and line.startswith("╰"):
            assert opened and line.endswith("╯") and display_width(line) == width, (line, opened)
            opened = False
        elif unicode and line.startswith(("│", "├")):
            assert opened and line.endswith("│" if line.startswith("│") else "┤"), ("bad card row", line, opened)
            assert display_width(line) == width, ("bad card row width", line, width)
        elif not unicode and line.startswith("+") and line.endswith("+"):
            assert display_width(line) == width, text
            if set(line[1:-1]) == {"-"}:
                assert opened, text
                opened = False
            elif not opened:
                opened = True
        elif not unicode and line.startswith("|"):
            assert opened and line.endswith("|") and display_width(line) == width, text
    assert not opened, ("unclosed terminal card", text[:400])
class BatchInput:
    fd = 0
    def __init__(self, batches):
        self.batches = list(batches)
    def __enter__(self): return self
    def __exit__(self, *args): pass
    def discard_clicks(self): pass
    def wait(self, seconds): return self.batches.pop(0) if self.batches else [("q",)]
class ImmediateRead:
    pending = False
    def start(self, reader):
        self.result, self.pending = reader(), True
    def take(self):
        ready, self.pending = self.pending, False
        return ready, self.result if ready else None
class ResponsiveInput:
    fd = 0
    def __init__(self, *args, **kwargs):
        self.events = iter([[(pressed,)], [("q",)]])
    def __enter__(self):
        return self
    def __exit__(self, *args):
        pass
    def discard_clicks(self):
        pass
    def wait(self, seconds):
        assert entered.wait(1), "background read never started"
        assert not released.is_set(), "read completed before input was handled"
        return next(self.events)
msg_members = [{"participant": "m1", "role": "main", "joined": True, "model": "claude-opus-5-5", "seq": 1, "provider": "claude", "attempt": "a1"},
               {"participant": "m2", "role": "main", "joined": True, "model": "gpt-6.1-sol", "seq": 2, "provider": "codex", "attempt": "a2"},
               {"participant": "m3", "role": "main", "joined": True, "model": "gpt-6-luna", "seq": 3, "provider": "codex", "attempt": "a3"}]
msg_log = [{"id": "n%d" % i, "sender": "m1" if i % 3 else "m3", "recipient": "m2" if i % 3 else "m1", "targets": ["m2" if i % 3 else "m1"],
            "message_kind": "question" if i % 5 == 0 else "note", "ts": "2026-10-0%dT10:%02d:00Z" % (6 if i < 5 else 7, i),
            "pending_for": ["m2"] if i == 29 else [], "text": "line %d\nsecond \x1b[31mred" % i} for i in range(30)]
msg_report = {"collection": {"ok": True}, "attempts": {"active_recent": [
    {"attempt_id": "a%d" % n, "state": "working", "panel": {"room_id": "r", "room_participant": "m%d" % n, "role": "main"}} for n in (1, 2, 3)],
    "recent": [{"attempt_id": "c1", "tool": "panel-council", "task_id": "decide", "state": "done", "updated_at": "2026-10-07T10:00:00Z",
                "parent_attempt_id": "a2", "panel": {"role": "advisor", "label": "Panel attention signal", "room_id": "r", "room_participant": "m2"}}]},
              "goal": {"text": "Ship the tabbed panel", "source": "plan"},
              "room": {"id": "r", "title": "T", "participants": msg_members, "message_count": 30, "messages": msg_log[-12:],
                       "pairs": [{"recipient": "m2", "pending": 2}],
                       "repo_tasks": [{"id": "t-blk", "title": "Blocked one", "state": "blocked", "provider": "codex", "verify": "make a"},
                                      {"id": "t-rdy", "title": "Ready one", "state": "ready", "provider": "", "verify": "make b"},
                                      {"id": "t-run", "title": "Running one", "state": "running", "provider": "claude",
                                       "claimed_by_participant": "m1", "verify": "make c", "claim_expired": True, "claim_age_s": 32428},
                                      {"id": "t-ver", "title": "Verified one", "state": "done", "provider": "codex", "verify": "make d"},
                                      {"id": "t-rev", "title": "Review one", "state": "review", "provider": "claude", "verify": "make e"}]}}
summary_member = {"participant": "summary-worker", "role": "worker", "model": "gpt-6.1-sol", "state": "done", "exit": 0,
                  "title": "Bounded fix", "task_id": "summary-task", "joined_at": "2026-10-13T12:00:00Z"}
review_member = dict(summary_member, role="reviewer")

msg_log[28]["text"] = "long " + "word " * 80 + "END\n\nparagraph two"
msg_log[28]["pending_for"] = ["m2"]
pressed = "t"
