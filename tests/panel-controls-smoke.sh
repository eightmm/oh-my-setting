#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PYTHONDONTWRITEBYTECODE=1 python3 -I - "$ROOT" <<'PY'
import contextlib
import importlib
import sys
import types
sys.path.insert(0, sys.argv[1] + "/scripts/lib")
from dashboard_projection import display_width
import panel_controls as pc

CAPS_SHELL = {"focus": True, "read": True, "restart": False}
CAPS_JOB = {"logs": True, "interrupt": True, "stop": True, "restart": True}


class Shells:
    def __init__(self, rows):
        self.rows, self.calls = rows, []

    def list_terminals(self, repo):
        return list(self.rows)

    def open_terminal(self, repo, session, **kw):
        self.calls.append(("open", session, kw))

    def focus_terminal(self, repo, ident):
        self.calls.append(("focus", ident))

    def read_terminal(self, repo, ident, *, maximum=32768, lines=200):
        self.calls.append(("read", ident, maximum, lines))
        return "ok\x1b[31mred\x1b[0m\ttab"

    def forget_terminal(self, repo, ident):
        self.calls.append(("forget", ident))


class Jobs:
    def __init__(self, rows):
        self.rows, self.calls, self.truncated = rows, [], False

    def list_jobs(self, repo):
        return [dict(r) for r in self.rows]

    def run_job(self, repo, session, argv, *, label="Job", cwd=None):
        self.calls.append(("run", session, argv, label))

    def job_logs(self, repo, ident, generation, *, maximum=32768):
        self.calls.append(("logs", ident, generation, maximum))
        return {"text": "\n".join("line %d" % i for i in range(40)), "truncated": self.truncated, "retention": "latest"}

    def interrupt_job(self, repo, ident, generation):
        self.calls.append(("interrupt", ident, generation))

    def stop_job(self, repo, ident, generation):
        self.calls.append(("stop", ident, generation))

    def restart_job(self, repo, session, ident, generation):
        self.calls.append(("restart", session, ident, generation))

    def forget_job(self, repo, ident, generation):
        self.calls.append(("forget", ident, generation))


def job(ident, state="running", gen=3, label=None, **extra):
    return dict({"id": ident, "label": label or ident, "kind": "job", "state": state, "generation": gen,
                 "exit_code": None, "capabilities": CAPS_JOB}, **extra)


def shell(ident, label=None, caps=CAPS_SHELL):
    return {"id": ident, "label": label or ident, "kind": "shell", "state": "live", "capabilities": caps}


@contextlib.contextmanager
def imports(table):
    """Lazy imports see only `table`; any other module name is simulated as missing."""
    seen, real = [], importlib.import_module

    def fake(name, package=None):
        seen.append(name)
        if name in table:
            return table[name]
        raise ModuleNotFoundError("No module named %r" % name)

    importlib.import_module = fake
    try:
        yield seen
    finally:
        importlib.import_module = real


FRAMES = []


def drive(inputs, shells, jobs, modules=None, **kw):
    out, prompts, it = [], [], iter(inputs)

    def reader(prompt):
        prompts.append(prompt)
        try:
            value = next(it)
        except StopIteration:
            raise EOFError
        return value() if callable(value) else value

    with imports(modules or {}):
        pc.browse_controls("repo", reader=reader, writer=out.append, shells=shells, jobs=jobs, **kw)
    FRAMES[:] = out
    return "\n".join(out)


def result_frame():
    """The frame written right after the action menu of the last drive()."""
    return FRAMES[[i for i, f in enumerate(FRAMES) if f.startswith(("l ", "f ", "r "))][0] + 1]


def check(cond, message):
    if not cond:
        raise SystemExit("panel-controls-smoke: FAIL " + message)


# Completed jobs collapse by default, unknown stays, toggle shows them; listing and exit do nothing.
sh, jb = Shells([shell("t1")]), Jobs([job("j1"), job("j2", "completed"), job("j3", "weird\x1b[2J")])
text = drive(["f", "f", "q"], sh, jb)
first, second, third = text.split("Terminals and jobs")[1:]
check("1 finished hidden (f shows)" in first and "j2" not in first, "completed jobs not collapsed")
check("j3" in first and "weird?[2J" in first and "\x1b" not in text, "unknown job hidden or unsanitized")
check("j2" in second and "1 finished shown (f hides)" in second, "toggle did not show finished jobs")
check("j2" not in third, "second toggle did not collapse")
check("1 finished hidden (f shows)" in third, "collapsed count changed")
check(sh.calls == [] and jb.calls == [], "listing or exit invoked backend")

# Pagination and display width stay inside the caller's box.
many = Shells([shell("t%d" % i, "終端%d 한글한글한글한글" % i) for i in range(12)])
text = drive(["n", "n", "p", "q"], many, Jobs([]), width=30, height=8)
check(all(display_width(line) <= 30 for line in text.splitlines()), "line wider than width")
check("page 1/2" in text and "page 2/2" in text and "page 3" not in text, "pages not shown")
check(max(len(s.splitlines()) for s in text.split("Terminals and jobs")[1:]) <= 8, "page taller than height")

# Unsupported/failing backend is a truthful partial listing.
class Broken:
    def list_jobs(self, repo):
        raise RuntimeError("boom\x1b[0m")

text = drive(["q"], None, Broken())
check("shells unsupported" in text and "jobs unavailable (boom?[0m)" in text, "partial warning missing")

# Explicit shell actions delegate; read output is sanitized; unavailable capability is refused.
sh = Shells([shell("t1"), shell("t2", caps={"focus": False, "read": False, "restart": False})])
text = drive(["1", "f", "1", "r", "2", "r", "q"], sh, Jobs([]))
check(sh.calls == [("focus", "t1"), ("read", "t1", 32768, 200)], "shell delegation wrong: %r" % sh.calls)
check("ok?[31mred?[0m tab" in text and "No actions available." in text and "Action not available." in text,
      "shell output/capability handling wrong")

# Job actions pass the exact generation; the list is re-read first.
jb = Jobs([job("j1", gen=3)])
text = drive(["1", "l", "1", "i", "1", "s", "q"], Shells([]), jb)
check(jb.calls == [("logs", "j1", 3, 32768), ("interrupt", "j1", 3), ("stop", "j1", 3)], "job calls %r" % jb.calls)
check("earlier lines omitted" in text and "line 39" in text, "log tail not bounded")

# Stale selection: new generation, vanished id, or id reused by another label never gets substituted.
for change in (lambda: jb.rows.__setitem__(0, job("j1", gen=4)),
               lambda: jb.rows.__setitem__(0, job("j9", gen=3, label="j1")),
               lambda: jb.rows.clear()):
    jb = Jobs([job("j1", gen=3)])
    text = drive(["1", lambda: (change(), "s")[1], "q"], Shells([]), jb)
    check(jb.calls == [] and "stale" in text, "stale selection acted")

# Restart: warns, cancel does nothing, yes passes exact args, no session refuses before asking.
jb = Jobs([job("j1", gen=3)])
text = drive(["1", "r", "no", "q"], Shells([]), jb, session="s1")
check("latest generation's log is kept" in text and "Restart cancelled." in text and jb.calls == [], "restart cancel")
check("r restart (loses log)" in text, "restart menu lacks log-loss warning")
drive(["1", "r", "yes", "q"], Shells([]), jb, session="s1")
check(jb.calls == [("restart", "s1", "j1", 3)], "restart args %r" % jb.calls)

# Forget exposes permanent deletion only for eligible rows and requires an explicit yes.
sh = Shells([dict(shell("gone"), state="gone", capabilities={"forget": True})])
jb = Jobs([job("done", "exited", **{"capabilities": {"forget": True}})])
text = drive(["1", "x", "no", "f", "2", "x", "yes", "q"], sh, jb)
check("permanently deletes" in text and "Forget cancelled." in text and
      sh.calls == [] and jb.calls == [("forget", "done", 3)], "forget confirmation/cancellation")
text = drive(["1", "x", "yes", "q"], sh, Jobs([]))
check(sh.calls == [("forget", "gone")], "gone shell forget")
sh = Shells([shell("live")])
text = drive(["1", "x", "q"], sh, Jobs([]))
check(sh.calls == [] and "Action not available." in text, "live shell forget offered")
jb = Jobs([job("j1", gen=3)])
text = drive(["1", "r", "q"], Shells([]), jb)
check("requires an explicit session" in text and jb.calls == [], "restart without session")

# Launching: argv via shlex, shell syntax literal, bad input refused, session required.
jb, sh = Jobs([]), Shells([])
drive(["r", "python3 -c 'print(1)' \"x y\"", "mine", "r", "echo $HOME; ls", "", "r", "'open", "r", "  ", "o", "", "q"],
      sh, jb, session="s1")
check(jb.calls == [("run", "s1", ["python3", "-c", "print(1)", "x y"], "mine"),
                   ("run", "s1", ["echo", "$HOME;", "ls"], "Job")], "run_job calls %r" % jb.calls)
check(sh.calls == [("open", "s1", {"label": "Terminal"})], "open_terminal %r" % sh.calls)
jb, sh = Jobs([]), Shells([])
text = drive(["r", "o", "q"], sh, jb)
check(jb.calls == [] and sh.calls == [] and text.count("requires an explicit session") == 2, "launch without session")

# Default lazy imports use the real backend module names and dispatch to them; injected ones win.
fake_sh, fake_jb = Shells([shell("t1")]), Jobs([job("j1", gen=2)])
mods = {"panel_terminals": fake_sh, "panel_jobs": fake_jb}
with imports(mods) as seen:
    script = iter(["1", "f", "2", "l", "q"])
    pc.browse_controls("repo", reader=lambda prompt: next(script), writer=lambda text: None)
check(sorted(set(seen)) == ["panel_jobs", "panel_terminals"], "default backends imported as %r" % seen)
check(fake_sh.calls == [("focus", "t1")] and fake_jb.calls == [("logs", "j1", 2, 32768)], "default dispatch")
check((pc.SHELL_BACKEND, pc.JOB_BACKEND) == ("panel_terminals", "panel_jobs"), "backend constants")
text = drive(["q"], None, None)
check("shells unsupported" in text and "jobs unsupported" in text, "simulated missing modules not reported")

# Generation must be an exact positive int; anything else is listed but never controlled.
for bad in (None, 0, -2, True, "3", 3.0, [3]):
    row = job("j1")
    if bad is None:
        del row["generation"]
    else:
        row["generation"] = bad
    jb = Jobs([row])
    text = drive(["1", "s", "1", "l", "1", "i", "q"], Shells([]), jb, session="s1")
    check(jb.calls == [] and "generation is unknown" in text and "g?" in text, "bad generation %r acted: %r" % (bad, jb.calls))
jb = Jobs([job("j1", gen=True)])
drive(["1", "r", "yes", "q"], Shells([]), jb, session="s1")
check(jb.calls == [], "bool generation restarted")

# Requested dimensions are respected without enlargement, however many notes and malformed rows.
class Noisy:
    def list_terminals(self, repo):
        return [None, {"id": ""}, 7]

    def list_jobs(self, repo):
        return "nope"

for shells_api, jobs_api in ((Noisy(), Noisy()), (None, None), (Shells([shell("t1", "終端" * 20)]), Jobs([job("j1")]))):
    for w in list(range(1, 14)) + [30]:
        for h in range(1, 10):
            drive(["1", "1", "n", "q"], shells_api, jobs_api, width=w, height=h)
            for frame in FRAMES[:1]:
                check(len(frame.splitlines()) <= h and all(pc.display_width(l) <= w for l in frame.splitlines()),
                      "frame %dx%d exceeded: %r" % (w, h, frame))
            check(all(pc.display_width(l) <= w for f in FRAMES for l in f.splitlines()), "output wider than %d: %r" % (w, FRAMES))
text = drive(["q"], Noisy(), Noisy(), width=8, height=1)
check(text.count("\n") == 0 and pc.display_width(text) <= 8, "8x1 frame: %r" % text)
text = drive(["q"], Noisy(), Noisy(), width=90, height=4)
check(len(text.splitlines()) == 4 and "more notes omitted" in text and "[partial]" in text, "notes not bounded: %r" % text)

# Read/log views keep the backend's truncated warning inside the display; untruncated views never claim it.
jb = Jobs([job("j1")])
text = drive(["1", "l", "q"], Shells([]), jb, width=40, height=6)
check("Truncated" not in text, "untruncated view warned")
jb.truncated = True
for w, h in ((40, 6), (8, 2), (8, 1), (8, 4)):
    drive(["1", "l", "q"], Shells([]), jb, width=w, height=h)
    view = result_frame()
    check(view.startswith("Trunc") and len(view.splitlines()) <= h and all(pc.display_width(l) <= w for l in view.splitlines()),
          "truncated warning %dx%d: %r" % (w, h, view))
drive(["1", "l", "q"], Shells([]), jb, width=30, height=6)
check("earlier lines omitted" in result_frame() and "line 39" in result_frame() and len(result_frame().splitlines()) == 6,
      "log tail %r" % result_frame())

# Numeric selection is length-bounded before int(); thousands of digits are refused without crash or action.
sh = Shells([shell("t1")])
text = drive(["9" * 5000, "1" * 40, "0", "1", "", "q"], sh, Jobs([]))
check(text.count("Unknown command.") == 3 and sh.calls == [], "oversized selection %r" % sh.calls)

# Restart warning is shown before the confirmation; too small a display refuses instead of hiding it.
jb = Jobs([job("j1", gen=3)])
text = drive(["1", "r", "yes", "q"], Shells([]), jb, session="s1", width=10, height=1)
check(jb.calls == [] and "Restart" in text and "cancelled" not in text, "restart on tiny display")
jb = Jobs([job("j1", gen=3)])
text = drive(["1", "r", "yes", "q"], Shells([]), jb, session="s1", width=24, height=8)
check(jb.calls == [("restart", "s1", "j1", 3)] and "current log: only the" in text, "wrapped warning restart")

print("panel-controls-smoke: ok")
PY
