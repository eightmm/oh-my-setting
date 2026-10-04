#!/usr/bin/env python3
"""Terminal front end; existing OMS tools retain state and mutation authority."""

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import uuid

from dashboard_projection import clean, fit

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / "scripts" / "oms"
PROVIDERS = ("codex", "claude")
MAX_PROMPT = 65536
SESSION_ID = r"[A-Za-z0-9][A-Za-z0-9_-]{0,159}"


def repository(value):
    path = Path(value).resolve(strict=True)
    if not path.is_dir():
        raise ValueError("repository must be a directory")
    result = subprocess.run(
        ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, timeout=5, check=False, stdin=subprocess.DEVNULL,
    )
    return Path(result.stdout.strip()).resolve() if result.returncode == 0 else path


def child_environment(provider, repo):
    env = os.environ.copy()
    env["OMS_AGENT"] = provider
    env["OMS_PANEL_REPO"] = str(repo)
    env["OMS_PANEL_ENTRYPOINT"] = str(ENTRY)
    env["PATH"] = str(ENTRY.parent) + os.pathsep + env.get("PATH", "")
    return env


def bootstrap(provider, repo):
    peer = "claude" if provider == "codex" else "codex"
    return (
        "You are the top-level %s session opened from the OMS terminal panel. "
        "Repository path (data) is %s. Use the oms on PATH (%s), not a different "
        "installed revision. Follow project instructions and the user's scope. "
        "Launching this session does not authorize unrelated paid calls. "
        "When the user requests collaboration, you can orchestrate %s: "
        "oms consult --to %s --repo . --prompt TEXT for read-only judgment; "
        "oms peer-delegate --to %s --repo . --brief-file PATH --verify COMMAND "
        "for bounded implementation; oms peer-review --providers %s --repo . "
        "--prompt TEXT --gate --verify COMMAND for review. "
        "Use fresh scoped briefs with paths, authority, constraints, success "
        "criteria and a return contract. Inspect the returned artifacts and "
        "patch, use patch-admit/patch-land when authorized, and verify the result. "
        "Worker completion is not acceptance. Workers cannot recursively "
        "orchestrate. Do not commit, push or install without user authority. "
        "OMS records are shared; native Claude/Codex conversations are separate. "
        "If no task has been supplied, report readiness and wait."
    ) % (provider, json.dumps(str(repo)), json.dumps(str(ENTRY)), peer, peer, peer, peer)


def native_command(provider, repo, resume=None, model=None):
    command = [provider]
    if provider == "codex":
        if resume:
            command += ["resume", resume]
        if model:
            command += ["--model", model]
        # A first user turn preserves configured developer/system instructions.
        command += [bootstrap(provider, repo)]
    else:
        if resume:
            command += ["--resume", resume]
        if model:
            command += ["--model", model]
        command += ["--append-system-prompt", bootstrap(provider, repo)]
    return command


def peer_command(action, provider, repo, prompt=None, brief=None, verify=None):
    peer = "claude" if provider == "codex" else "codex"
    prefix = ["bash", str(ENTRY)]
    if action == "ask":
        return prefix + ["consult", "--repo", str(repo), "--to", peer,
                         "--prompt", prompt]
    if action == "delegate":
        if not verify or not verify.strip():
            raise ValueError("implementation needs an explicit verifier")
        return prefix + ["peer-delegate", "--repo", str(repo), "--to", peer,
                         "--brief-file", str(brief), "--verify", verify,
                         "--role", "implementation-worker"]
    if action == "review":
        if not verify or not verify.strip():
            raise ValueError("review needs an explicit verifier")
        return prefix + ["peer-review", "--repo", str(repo), "--providers", peer,
                         "--prompt", prompt, "--gate", "--verify", verify]
    raise ValueError("unknown peer action")


def run(command, repo, provider=None):
    return subprocess.call(command, cwd=str(repo),
                           env=child_environment(provider, repo) if provider else None)


def dashboard(repo, as_json=False):
    command = ["bash", str(ENTRY), "dashboard", "--repo", str(repo)]
    if as_json:
        command += ["--json"]
    result = subprocess.run(command, cwd=str(repo), capture_output=True,
                            text=True, timeout=30, check=False, stdin=subprocess.DEVNULL)
    if as_json:
        return json.loads(result.stdout), result.returncode
    return result.stdout, result.returncode


def tmux_command(*args):
    return ["tmux"] + list(args)


def shell_command(command):
    # tmux shell-command is a shell string, unlike the native subprocess argv.
    return "exec " + " ".join(shlex.quote(str(arg)) for arg in command)


def panel_shell(repo, session):
    return shell_command(["env", "OMS_PANEL_SESSION=" + session,
                          "bash", str(ROOT / "scripts" / "panel.sh"),
                          "--repo", str(repo), "--layout", "inline"])


def watch_shell(repo):
    return shell_command(["bash", str(ENTRY), "dashboard", "--repo", str(repo), "--watch"])


def split_panel(repo):
    session = "oms-panel-" + uuid.uuid4().hex[:12]
    env = os.environ.copy()
    env["OMS_PANEL_SESSION"] = session
    created = False
    try:
        pane = subprocess.check_output(tmux_command("new-session", "-d", "-P", "-F", "#{pane_id}",
                                      "-s", session, "-n", "control", "-c", str(repo),
                                      panel_shell(repo, session)), env=env, text=True).strip()
        created = True
        subprocess.run(tmux_command("set-environment", "-t", session,
                                    "OMS_PANEL_SESSION", session), check=True)
        subprocess.run(tmux_command("split-window", "-h", "-l", "40%", "-t",
                                    pane, "-c", str(repo), watch_shell(repo)),
                       check=True)
        subprocess.run(tmux_command("select-pane", "-t", pane), check=True)
        attach = "switch-client" if os.environ.get("TMUX") else "attach-session"
        result = subprocess.call(tmux_command(attach, "-t", session))
        print("OMS session %s remains available while its panes are running." % session)
        return result
    except (OSError, subprocess.CalledProcessError):
        # Only failed construction is rolled back; detaching must preserve work.
        if created:
            subprocess.run(tmux_command("kill-session", "-t", session), check=False)
        raise


def open_native(provider, repo, resume=None, model=None):
    if not shutil.which(provider):
        raise ValueError("%s is not installed on PATH" % provider)
    session = os.environ.get("OMS_PANEL_SESSION", "")
    if os.environ.get("TMUX") and re.fullmatch(r"oms-panel-[a-f0-9]{12}", session):
        command = ["bash", str(ROOT / "scripts" / "panel.sh"), "--repo", str(repo),
                   "--layout", "inline", "--launch", provider]
        if resume:
            command += ["--resume", resume]
        if model:
            command += ["--model", model]
        # A new window preserves existing sessions, including active tool loops.
        window = subprocess.check_output(
            tmux_command("new-window", "-P", "-F", "#{pane_id}", "-t", session,
                         "-n", provider, "-c", str(repo), shell_command(command)), text=True,
        ).strip()
        subprocess.run(tmux_command("split-window", "-h", "-l", "35%", "-t", window,
                                    "-c", str(repo), watch_shell(repo)), check=True)
        subprocess.run(tmux_command("select-pane", "-t", window), check=True)
        return 0
    return run(native_command(provider, repo, resume, model), repo, provider)


def read_field(label, required=True):
    text = input(label).strip()
    if len(text.encode("utf-8")) > MAX_PROMPT:
        raise ValueError("input exceeds 64 KiB")
    if required and not text:
        raise ValueError("input is required")
    if any(ord(char) < 32 for char in text):
        raise ValueError("input must be a single printable line")
    return text


def show(repo, provider, previous):
    width = max(30, min(200, shutil.get_terminal_size((100, 28)).columns))
    if sys.stdout.isatty() and os.environ.get("TERM") not in (None, "dumb"):
        print("\033[H\033[2J", end="")
    print(fit("OMS control panel | %s | main=%s" % (clean(repo.name), provider), width))
    availability = "  ".join("%s=%s" % (name, "installed" if shutil.which(name) else "missing")
                             for name in PROVIDERS)
    print(fit(availability + " | " + previous, width))
    try:
        text, status = dashboard(repo)
        # Keep the menu usable in a small pane; detailed state has its own view.
        wanted = ("goal:", "task:", "plan:", "attempts:", "acceptance:",
                  "attention:", "COLLECTION FAILED", "state:")
        for line in text.splitlines():
            if line.startswith(wanted):
                print(fit(line, width))
        if status:
            print("State collection degraded; open Details for the affected source.")
    except (OSError, ValueError, subprocess.TimeoutExpired):
        print("State temporarily unavailable; sessions remain accessible.")
    for line in ("1 Codex  2 Claude  3 Resume  4 Main",
                 "5 Ask peer  6 Implement  7 Review",
                 "8 Details  9 Refresh  q Quit/detach"):
        print(fit(line, width))
    if os.environ.get("OMS_PANEL_SESSION"):
        print(fit("tmux: Ctrl-b w choose window | Ctrl-b n next | Ctrl-b d detach", width))
    print(fit("Peer calls require your task; patches return for admission and verification.", width))


def interactive(repo):
    provider = os.environ.get("OMS_AGENT", "codex")
    if provider not in PROVIDERS:
        provider = "codex"
    previous = "ready"
    while True:
        show(repo, provider, previous)
        try:
            choice = read_field("oms> ")
            if choice == "q":
                session = os.environ.get("OMS_PANEL_SESSION", "")
                if os.environ.get("TMUX") and re.fullmatch(r"oms-panel-[a-f0-9]{12}", session):
                    subprocess.run(tmux_command("detach-client", "-s", session), check=True)
                    previous = "detached; existing sessions preserved"
                    continue
                return 0
            if choice in ("1", "2", "3"):
                selected = PROVIDERS[int(choice) - 1] if choice != "3" else read_field("Provider (codex/claude): ")
                if selected not in PROVIDERS:
                    raise ValueError("select codex or claude")
                resume = read_field("Exact session ID: ") if choice == "3" else None
                if resume and not re.fullmatch(SESSION_ID, resume):
                    raise ValueError("invalid session ID")
                provider = selected
                status = open_native(provider, repo, resume)
                if os.environ.get("TMUX") and os.environ.get("OMS_PANEL_SESSION") and status == 0:
                    previous = "%s window opened; native outcome unreported" % provider
                else:
                    previous = "%s native session exit=%s" % (provider, status)
            elif choice == "4":
                selected = read_field("Main provider (codex/claude): ")
                if selected not in PROVIDERS:
                    raise ValueError("select codex or claude")
                provider = selected
                previous = "main provider selected"
            elif choice in ("5", "6", "7"):
                action = {"5": "ask", "6": "delegate", "7": "review"}[choice]
                peer = "claude" if provider == "codex" else "codex"
                if not shutil.which(peer):
                    raise ValueError("%s is not installed on PATH" % peer)
                print("%s -> %s (%s)" % (provider, peer, action))
                prompt = brief = verify = None
                if action == "delegate":
                    brief = Path(read_field("Scoped brief file: ")).expanduser()
                    if not brief.is_absolute():
                        brief = repo / brief
                    brief = brief.resolve(strict=True)
                    if not brief.is_file() or brief.stat().st_size > MAX_PROMPT:
                        raise ValueError("brief must be a regular file of at most 64 KiB")
                else:
                    prompt = read_field("Question / review task: ")
                if action in ("delegate", "review"):
                    verify = read_field("Verification command: ")
                command = peer_command(action, provider, repo, prompt, brief, verify)
                status = run(command, repo, provider)
                previous = "%s -> %s %s exit=%s (not acceptance)" % (provider, peer, action, status)
                read_field("Press Enter to return to the panel: ", required=False)
            elif choice == "8":
                run(["bash", str(ENTRY), "dashboard", "--repo", str(repo)], repo)
                read_field("Press Enter to return: ", required=False)
            elif choice != "9":
                raise ValueError("choose 1-9 or q")
        except EOFError:
            return 0
        except KeyboardInterrupt:
            previous = "interrupted; inspect the recorded attempt before retrying"
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            previous = "error: " + clean(str(error), 140)


def main(argv=None):
    parser = argparse.ArgumentParser(description="OMS terminal control panel for Codex and Claude Code.")
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--layout", choices=("auto", "inline", "split"), default="auto")
    parser.add_argument("--launch", choices=PROVIDERS, help="open one top-level native session")
    parser.add_argument("--resume", help="resume an exact native session ID (requires --launch)")
    parser.add_argument("--model", help="explicit native model (requires --launch); default preserves provider settings")
    parser.add_argument("--json", action="store_true", help="read-only availability and shared OMS state")
    parser.add_argument("--dry-run", action="store_true", help="show the launch plan without starting anything")
    args = parser.parse_args(argv)
    if (args.resume or args.model) and not args.launch:
        parser.error("--resume and --model require --launch")
    if args.json and args.launch:
        parser.error("--json is read-only; use --dry-run for a launch plan")
    if args.json and args.dry_run:
        parser.error("choose --json or --dry-run")
    if args.resume and not re.fullmatch(SESSION_ID, args.resume):
        parser.error("invalid exact session ID")
    if args.model and (args.model.startswith("-") or len(args.model) > 160
                       or not re.fullmatch(r"[A-Za-z0-9_./:-]+", args.model)):
        parser.error("invalid model ID")
    if not (args.json or args.dry_run) and (
            os.environ.get("OMS_HARNESS_CHILD") == "1"
            or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"):
        parser.error("a worker cannot open owner sessions or orchestrate peers; return the need to its parent")
    try:
        repo = repository(args.repo)
        if args.json:
            state, status = dashboard(repo, as_json=True)
            print(json.dumps({"schema": 1, "kind": "oms-panel", "dashboard": state,
                              "providers": {name: {"installed": bool(shutil.which(name))}
                                            for name in PROVIDERS},
                              "split_available": bool(shutil.which("tmux")),
                              "coverage": "OMS records; native sessions are separate"}))
            return status
        split = not args.launch and (args.layout == "split" or (
            args.layout == "auto" and shutil.which("tmux")
            and not os.environ.get("TMUX") and os.name != "nt"))
        if args.dry_run:
            print(json.dumps({"schema": 1, "kind": "oms-panel-launch",
                              "repo": str(repo), "layout": "split" if split else "inline",
                              "provider": args.launch,
                              "argv": native_command(args.launch, repo, args.resume, args.model)
                              if args.launch else None,
                              "environment_keys": ["OMS_AGENT", "OMS_PANEL_REPO", "OMS_PANEL_ENTRYPOINT", "PATH"],
                              "executes": False}))
            return 0
        if not sys.stdin.isatty() or not sys.stdout.isatty():
            parser.error("interactive panel needs a terminal; use --json or --dry-run")
        if args.launch:
            if not shutil.which(args.launch):
                raise ValueError("%s is not installed on PATH" % args.launch)
            status = run(native_command(args.launch, repo, args.resume, args.model), repo, args.launch)
            if os.environ.get("TMUX") and os.environ.get("OMS_PANEL_SESSION"):
                print("%s session exited (%s)." % (args.launch, status))
                try:
                    read_field("Press Enter to close this pane: ", required=False)
                except EOFError:
                    pass
            return status
        if split:
            if not shutil.which("tmux"):
                raise ValueError("split layout needs an installed tmux; inline needs no extra dependency")
            return split_panel(repo)
        return interactive(repo)
    except KeyboardInterrupt:
        return 130
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print("error: " + clean(str(error), 200), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
