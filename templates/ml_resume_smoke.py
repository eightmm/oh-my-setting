"""Resume smoke: an interrupted run resumed from `latest` must continue
exactly as an uninterrupted run would. CPU-only, stdlib only, seconds.

Run via the verification contract (after scripts/ml_smoke.py):

    bash scripts/check.sh ml-smoke
    python3 scripts/ml_resume_smoke.py [--self-test] [--torch]

The harness takes a plug-in with four callables for this project:

    build()            -> state    all mutable training state (model, optimizer,
                                   scheduler, RNG streams, step counter)
    step(state)        -> loss     advance exactly one training step
    save(state, path)              write the whole state to `path`
    load(path)         -> state    rebuild the state from `path` alone
    parts(state)       -> dict     comparable views: step, optimizer, scheduler,
                                   rng (any value; compared with repr())

It then (a) runs N steps straight through, (b) runs K steps, saves `latest`
atomically (temp file in the same directory, then rename), discards the state,
loads, runs the remaining N-K steps, and (c) asserts both runs agree step for
step on the loss and on a digest of the final state, and that the step counter,
optimizer, scheduler and RNG stream continued rather than restarted. It also
asserts an interrupted save (only the temp file left) never leaves a truncated
`latest`.

This checks the project's plug-ins on a toy run. It is not a preempted Slurm
job. Replace `project_plugin()` below with the real plug-in; the built-in demo
only proves the harness itself.
"""

import argparse
import hashlib
import json
import os
import random
import sys
import tempfile

TOTAL_STEPS = 20
SAVE_AT = 7
PART_KEYS = ("step", "optimizer", "scheduler", "rng")


class Plugin:
    def __init__(self, build, step, save, load, parts, digest=None):
        self.build = build
        self.step = step
        self.save = save
        self.load = load
        self.parts = parts
        self._digest = digest

    def digest(self, state, workdir):
        if self._digest is not None:
            return self._digest(state)
        probe = os.path.join(workdir, "digest.probe")
        self.save(state, probe)
        with open(probe, "rb") as handle:
            return hashlib.sha256(handle.read()).hexdigest()


def atomic_save(plugin, state, path):
    """Write beside `path`, then rename: readers see the old file or the new."""
    tmp = path + ".tmp"
    plugin.save(state, tmp)
    with open(tmp, "rb") as handle:
        os.fsync(handle.fileno())
    os.replace(tmp, path)


def run_steps(plugin, state, count):
    return [plugin.step(state) for _ in range(count)]


def views(plugin, state):
    parts = plugin.parts(state)
    missing = [key for key in PART_KEYS if key not in parts]
    if missing:
        raise ValueError("parts() must return keys %s; missing %s" % (PART_KEYS, missing))
    return {key: repr(parts[key]) for key in PART_KEYS}


def check_interrupted_save(plugin, workdir, problems):
    """A save that dies before the rename leaves latest absent or intact."""
    latest = os.path.join(workdir, "interrupt", "latest.ckpt")
    os.makedirs(os.path.dirname(latest))
    state = plugin.build()
    run_steps(plugin, state, 3)
    # Case 1: no checkpoint yet; the first save is interrupted.
    plugin.save(state, latest + ".tmp")
    _truncate(latest + ".tmp")
    if os.path.exists(latest):
        problems.append("interrupted first save left a `latest` file behind")
    # Case 2: a good `latest` exists; the next save is interrupted.
    atomic_save(plugin, state, latest)
    good = _read(latest)
    good_views = views(plugin, plugin.load(latest))
    run_steps(plugin, state, 2)
    plugin.save(state, latest + ".tmp")
    _truncate(latest + ".tmp")
    if _read(latest) != good:
        problems.append("interrupted save changed `latest`")
    elif views(plugin, plugin.load(latest)) != good_views:
        problems.append("`latest` no longer loads to the last complete state")


def _read(path):
    with open(path, "rb") as handle:
        return handle.read()


def _truncate(path):
    size = os.path.getsize(path)
    with open(path, "r+b") as handle:
        handle.truncate(size // 2)


def check_resume(plugin, total=TOTAL_STEPS, save_at=SAVE_AT):
    """Return a list of human-readable problems; empty means resume is exact."""
    problems = []
    with tempfile.TemporaryDirectory(prefix="oms-resume-smoke-") as workdir:
        # (a) straight through
        ref = plugin.build()
        ref_losses = run_steps(plugin, ref, save_at)
        ref_at_save = views(plugin, ref)
        ref_losses += run_steps(plugin, ref, total - save_at)
        ref_views = views(plugin, ref)
        ref_digest = plugin.digest(ref, workdir)

        # (b) interrupted at `save_at`, resumed from latest
        latest = os.path.join(workdir, "run", "latest.ckpt")
        os.makedirs(os.path.dirname(latest))
        first = plugin.build()
        fresh_views = views(plugin, first)
        got_losses = run_steps(plugin, first, save_at)
        atomic_save(plugin, first, latest)
        del first
        if os.path.exists(latest + ".tmp"):
            problems.append("atomic save left a temp file after the rename")
        resumed = plugin.load(latest)
        loaded_views = views(plugin, resumed)
        got_losses += run_steps(plugin, resumed, total - save_at)
        got_views = views(plugin, resumed)
        got_digest = plugin.digest(resumed, workdir)

        # (c) compare
        for key in PART_KEYS:
            if loaded_views[key] != ref_at_save[key]:
                problems.append("%s was not restored at step %d" % (key, save_at))
            if loaded_views[key] == fresh_views[key] and ref_at_save[key] != fresh_views[key]:
                problems.append("%s restarted instead of continuing" % key)
            if got_views[key] != ref_views[key]:
                problems.append("%s differs at step %d after resume" % (key, total))
        for index, (want, got) in enumerate(zip(ref_losses, got_losses), 1):
            if want != got:
                problems.append("loss differs first at step %d: %r vs %r" % (index, want, got))
                break
        if got_digest != ref_digest:
            problems.append("final state digest differs after resume")

        check_interrupted_save(plugin, workdir, problems)
    return problems


# --- Built-in pure-Python demo plug-in -------------------------------------

TARGET = [1.0, -2.0, 0.5, 3.0]


def demo_plugin(omit=()):
    """A few floats, a seeded Random, momentum state, a step-decay schedule.

    `omit` names parts left out of the checkpoint (the load then restarts
    them), used by --self-test to build deliberately broken variants.
    """

    def fresh():
        return {
            "step": 0,
            "sched_step": 0,
            "w": [0.0] * len(TARGET),
            "velocity": [0.0] * len(TARGET),
            "rng": random.Random(1234),
        }

    def lr_at(step):
        return 0.1 * (0.5 ** (step // 5))

    def step(state):
        noise = [state["rng"].random() - 0.5 for _ in TARGET]
        grads = [2 * (w - t) + n for w, t, n in zip(state["w"], TARGET, noise)]
        lr = lr_at(state["sched_step"])
        state["velocity"] = [0.9 * v + g for v, g in zip(state["velocity"], grads)]
        state["w"] = [w - lr * v for w, v in zip(state["w"], state["velocity"])]
        state["step"] += 1
        state["sched_step"] += 1
        return sum((w - t) ** 2 for w, t in zip(state["w"], TARGET))

    def save(state, path):
        payload = {"step": state["step"], "w": state["w"]}
        if "optimizer" not in omit:
            payload["velocity"] = state["velocity"]
        if "scheduler" not in omit:
            payload["lr_step"] = state["sched_step"]
        if "rng" not in omit:
            payload["rng"] = state["rng"].getstate()
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, sort_keys=True)

    def load(path):
        with open(path, encoding="utf-8") as handle:
            payload = json.load(handle)
        state = fresh()
        state["w"] = payload["w"]
        state["step"] = payload["step"]
        if "velocity" in payload:
            state["velocity"] = payload["velocity"]
        if "rng" in payload:
            version, internal, gauss = payload["rng"]
            state["rng"].setstate((version, tuple(internal), gauss))
        if "lr_step" in payload:
            state["sched_step"] = payload["lr_step"]
        return state

    def parts(state):
        return {
            "step": state["step"],
            "optimizer": state["velocity"],
            "scheduler": state["sched_step"],
            "rng": state["rng"].getstate(),
        }

    return Plugin(fresh, step, save, load, parts)


def project_plugin():
    """TODO project: return Plugin(build, step, save, load, parts) for the
    real training code (a tiny config on CPU, a handful of steps)."""
    return demo_plugin()


# --- Optional torch adapter (not a dependency; skipped when absent) --------

def torch_plugin():
    import torch

    def build():
        torch.manual_seed(0)
        model = torch.nn.Linear(4, 1)
        optimizer = torch.optim.SGD(model.parameters(), lr=0.1, momentum=0.9)
        scheduler = torch.optim.lr_scheduler.StepLR(optimizer, step_size=5, gamma=0.5)
        return {"model": model, "opt": optimizer, "sched": scheduler, "step": 0}

    def step(state):
        x = torch.randn(8, 4)
        loss = ((state["model"](x) - x.sum(dim=1, keepdim=True)) ** 2).mean()
        state["opt"].zero_grad()
        loss.backward()
        state["opt"].step()
        state["sched"].step()
        state["step"] += 1
        return loss.item()

    def save(state, path):
        torch.save(
            {
                "model": state["model"].state_dict(),
                "opt": state["opt"].state_dict(),
                "sched": state["sched"].state_dict(),
                "step": state["step"],
                "rng": torch.get_rng_state(),
            },
            path,
        )

    def load(path):
        state = build()
        payload = torch.load(path, weights_only=False)
        state["model"].load_state_dict(payload["model"])
        state["opt"].load_state_dict(payload["opt"])
        state["sched"].load_state_dict(payload["sched"])
        state["step"] = payload["step"]
        torch.set_rng_state(payload["rng"])
        return state

    def parts(state):
        return {
            "step": state["step"],
            "optimizer": state["opt"].state_dict(),
            "scheduler": state["sched"].state_dict(),
            "rng": torch.get_rng_state().tolist(),
        }

    return Plugin(build, step, save, load, parts)


def self_test():
    """Each broken variant must be reported; a silent pass is the failure."""
    ok = True
    for omitted, expect in (("rng", "rng"), ("optimizer", "optimizer"), ("scheduler", "scheduler")):
        problems = check_resume(demo_plugin(omit=(omitted,)))
        hits = [problem for problem in problems if expect in problem]
        if hits:
            print("ml_resume_smoke self-test: %s omitted -> detected (%s)" % (omitted, hits[0]))
        else:
            print("ml_resume_smoke self-test: %s omitted -> NOT detected" % omitted, file=sys.stderr)
            ok = False
    return 0 if ok else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--self-test", action="store_true",
                        help="run broken demo variants and require the harness to flag them")
    parser.add_argument("--torch", action="store_true",
                        help="also run the optional torch adapter (skipped without torch)")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    failed = False
    plugins = [("project", project_plugin)]
    if args.torch:
        try:
            import torch  # noqa: F401
        except ImportError:
            print("ml_resume_smoke: torch adapter skipped (torch is not importable)")
        else:
            plugins.append(("torch", torch_plugin))
    for name, factory in plugins:
        problems = check_resume(factory())
        if problems:
            failed = True
            for problem in problems:
                print("ml_resume_smoke: %s: %s" % (name, problem), file=sys.stderr)
        else:
            print("ml_resume_smoke: %s: ok (resume from latest matches a straight run)" % name)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
