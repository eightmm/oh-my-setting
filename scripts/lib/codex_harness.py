"""Launch an existing harness with invocation-scoped Codex subscription inference."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import threading

from codex_auth_broker import AuthBroker, BridgeError, subscription_env
from codex_bridge import Gateway, SubscriptionBackend


def cli_command(host, model, tail, effort=None):
    if host == "codex":
        reasoning = ["-c", 'model_reasoning_effort="%s"' % effort] if effort else []
        return ["codex"] + reasoning + ["--model", model, "-c", 'model_provider="openai"'] + tail
    if host == "claude":
        return ["claude", "--model", model] + tail
    return ["pi", "--extension", str(Path(__file__).with_name("pi-codex-extension.mjs")),
            "--extension", str(Path(__file__).with_name("pi-oms-extension.mjs")),
            "--provider", "oms-codex", "--model", model] + (["--thinking", effort] if effort else []) + tail


def harness_env(host, gateway, model, window):
    env = subscription_env()
    env["OMS_MODEL_PROVIDER"] = "codex"
    env["OMS_MODEL_AUTH"] = "chatgpt-subscription"
    env["OMS_AGENT"] = host
    if host == "pi" and env.get("OMS_PANEL_MAIN_ATTEMPT") and env.get("OMS_PANEL_OWNER") == "codex":
        env["OMS_AGENT"] = "codex"
        env.pop("OMS_HOOK_AGENT", None)
    if host != "codex":
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
            env.pop(key, None)
        env["NO_PROXY"] = "127.0.0.1,localhost"
        env["no_proxy"] = "127.0.0.1,localhost"
        # Nested CLIs started by this host must not inherit its gateway route (see peer-common.sh).
        env["OMS_HARNESS_BRIDGE"] = "1"
    # Subscription only: no host keeps a metered or inherited Anthropic route.
    for key in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_CODE_USE_ANTHROPIC_AWS"):
        env.pop(key, None)
    if host == "claude":
        env["ANTHROPIC_AUTH_" "TOKEN"] = gateway.capability
        env.update(ANTHROPIC_BASE_URL=gateway.url, ANTHROPIC_MODEL=model, ANTHROPIC_DEFAULT_OPUS_MODEL=model,
                   ANTHROPIC_DEFAULT_SONNET_MODEL=model, ANTHROPIC_DEFAULT_HAIKU_MODEL=model,
                   CLAUDE_CODE_SUBAGENT_MODEL=model, ENABLE_TOOL_SEARCH="false",
                   CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
    elif host == "pi":
        env.update(OMS_CODEX_HARNESS_URL=gateway.url, OMS_CODEX_HARNESS_CAPABILITY=gateway.capability,
                   OMS_CODEX_HARNESS_MODEL=json.dumps({"id": model, "contextWindow": window}))
    return env


def validate_tail(host, tail):
    # A second routing option could silently select a different provider or billing mode.
    denied = {"--model", "-m", "--provider", "--api-key", "--models"}
    if host == "codex":
        denied.update(("--oss", "--local-provider", "--profile", "-p", "--config", "-c", "login", "logout"))
    if host == "claude":
        denied.add("--settings")
    for arg in tail:
        if arg == "--":
            break
        option = arg.split("=", 1)[0]
        if option in denied or any(arg.startswith(short) and arg != short for short in ("-m", "-c", "-p") if host == "codex"):
            raise BridgeError("Use the OMS model selection; native routing overrides are unsupported")


def record_panel_child(env, pid):
    import panel_release
    attempt, repo = env.get("OMS_PANEL_MAIN_ATTEMPT", ""), env.get("OMS_ROOM_REPO")
    if not repo or not panel_release.ID.fullmatch(attempt) or env.get("OMS_PANEL_HARNESS") != "pi":
        return
    try:
        row = json.loads(panel_release.events(repo, "show", "--attempt", attempt, "--json"))
        refs = row.get("refs") or {}
        if (row.get("tool") != "panel-main" or row.get("provider") != "codex"
                or refs.get("panel_role") != "main" or refs.get("panel_harness") != "pi"):
            raise ValueError("Pi panel ownership unavailable")
        native = panel_release.process_refs(pid, "native")
        if not native:
            raise ValueError("Pi child identity unavailable")
        panel_release.events(repo, "usage", "--attempt", attempt, "--actor", "panel-main",
                             *[arg for key, value in native.items() for arg in ("--ref", key + "=" + value)])
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print("OMS: Pi child identity unavailable; this main may need separate recovery.", file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Use Codex subscription models in Codex CLI, Pi, or Claude Code.",
        epilog="Pass native CLI arguments after --. --check makes no inference request. "
               "The Claude/Pi bridge is experimental; the selected harness owns tool permissions.")
    parser.add_argument("harness", choices=("codex", "pi", "claude"))
    parser.add_argument("--model", required=True, help="exact model ID in the installed Codex catalog")
    parser.add_argument("--effort", choices=("low", "medium", "high", "xhigh", "max"), default=None)
    parser.add_argument("--context-window", type=int, default=200000, help="Pi context estimate (default: conservative 200000)")
    parser.add_argument("--check", action="store_true", help="check executable, subscription login, exact model and bridge RPC")
    parser.add_argument("--dry-run", action="store_true", help="print launch argv without reading credentials or starting a CLI")
    supplied = list(sys.argv[1:] if argv is None else argv)
    separator = supplied.index("--") if "--" in supplied else len(supplied)
    args = parser.parse_args(supplied[:separator])
    tail = supplied[separator + 1:]
    broker = gateway = child = None
    try:
        if (os.environ.get("OMS_HARNESS_CHILD") == "1"
                or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"):
            raise BridgeError("A harness worker cannot launch another harness")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", args.model) or args.model in ("provider-default", "latest", "auto"):
            raise BridgeError("An exact model ID is required")
        if not 1024 <= args.context_window <= 2000000:
            raise BridgeError("context-window must be between 1024 and 2000000")
        validate_tail(args.harness, tail)
        if args.dry_run:
            print(json.dumps({"harness": args.harness, "model_provider": "codex", "auth": "chatgpt-subscription",
                              "model": args.model, "argv": cli_command(args.harness, args.model, tail, args.effort)}, ensure_ascii=False))
            return 0
        if not shutil.which("codex") or not shutil.which(args.harness):
            raise BridgeError("Both Codex CLI and the selected harness must already be installed")
        broker = AuthBroker()
        broker.credentials()
        model = broker.model(args.model)
        efforts = [row.get("reasoningEffort") for row in model.get("supportedReasoningEfforts") or []]
        effort = args.effort or model.get("defaultReasoningEffort")
        if effort is None:
            effort = next((level for level in ("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")
                           if level in efforts), None)
        if (effort is not None and not isinstance(effort, str)) or (efforts and effort not in efforts):
            raise BridgeError("The selected model does not support this reasoning effort")
        command = cli_command(args.harness, args.model, tail, effort)
        if args.check:
            print(json.dumps({"harness": args.harness, "model_provider": "codex", "auth": "chatgpt-subscription",
                              "model": args.model, "catalog_verified": True, "inference_verified": False}))
            return 0
        if args.harness != "codex":
            gateway = Gateway(SubscriptionBackend(broker), args.model, effort, efforts or [effort])
            threading.Thread(target=gateway.serve_forever, daemon=True).start()
        env = harness_env(args.harness, gateway, args.model, args.context_window)
        print("OMS: %s harness / Codex %s / existing ChatGPT subscription" % (args.harness, args.model), file=sys.stderr)
        if gateway:
            print("OMS: experimental local bridge; API-key fallback is disabled", file=sys.stderr)
        previous = {}
        for sig in (signal.SIGINT, signal.SIGTERM, getattr(signal, "SIGHUP", None)):
            if sig is not None:
                previous[sig] = signal.getsignal(sig)
                signal.signal(sig, (lambda _sig, _frame: None) if sig == signal.SIGINT else stop_signal)
        try:
            child = subprocess.Popen(command, env=env)
            if args.harness == "pi":
                record_panel_child(env, child.pid)
            return child.wait()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    except BridgeError as exc:
        print("error: " + str(exc), file=sys.stderr)
        return 2
    except OSError:
        print("error: cannot start the selected harness or Codex authentication broker", file=sys.stderr)
        return 2
    finally:
        if child and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=3)
        if gateway:
            gateway.close()
        if broker:
            broker.close()


def stop_signal(sig, _frame):
    raise SystemExit(128 + sig)


if __name__ == "__main__":
    sys.exit(main())
