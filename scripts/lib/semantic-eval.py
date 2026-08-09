#!/usr/bin/env python3
"""Deterministic patch checks plus a bounded, independent semantic verdict."""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple


SAFE_ID = re.compile(r"^[A-Za-z0-9_.-]{1,80}$")
MAX_INPUT_BYTES = 8 * 1024 * 1024
MAX_CHECK_OUTPUT_BYTES = 1024 * 1024


def fail(message: str) -> "NoReturn":
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def read_once(path: Path, label: str, limit: int = MAX_INPUT_BYTES) -> bytes:
    try:
        with path.open("rb") as handle:
            data = handle.read(limit + 1)
    except OSError as exc:
        fail("cannot read %s: %s" % (label, exc))
    if len(data) > limit:
        fail("%s exceeds the %d-byte limit" % (label, limit))
    return data


def load_object_bytes(data: bytes, label: str) -> Dict[str, Any]:
    try:
        value = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        fail("%s is not valid JSON" % label)
    if not isinstance(value, dict):
        fail("%s must be a JSON object" % label)
    return value


def repo_root(path: str) -> Path:
    try:
        result = subprocess.run(
            ["git", "-C", path, "rev-parse", "--show-toplevel"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("cannot resolve --repo: %s" % exc)
    if result.returncode != 0 or not result.stdout.strip():
        fail("--repo is not a git worktree")
    return Path(result.stdout.strip().rstrip("\r")).resolve()


def bounded_number(value: Any, label: str, low: float, high: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        fail("%s must be a number" % label)
    number = float(value)
    if number != number or number < low or number > high:
        fail("%s must be between %s and %s" % (label, low, high))
    return number


def validate_spec(spec: Dict[str, Any]) -> Dict[str, Any]:
    if spec.get("schema") != 1:
        fail("spec has an unsupported schema")
    eval_id = spec.get("id")
    if not isinstance(eval_id, str) or not SAFE_ID.fullmatch(eval_id):
        fail("spec.id is invalid")

    raw_checks = spec.get("checks", [])
    if not isinstance(raw_checks, list) or len(raw_checks) > 50:
        fail("spec.checks must be a list of at most 50 checks")
    checks = []
    check_ids = set()
    for index, raw in enumerate(raw_checks):
        if not isinstance(raw, dict):
            fail("spec.checks[%d] must be an object" % index)
        check_id = raw.get("id")
        if not isinstance(check_id, str) or not SAFE_ID.fullmatch(check_id):
            fail("spec.checks[%d].id is invalid" % index)
        if check_id in check_ids:
            fail("spec check ids must be unique")
        check_ids.add(check_id)
        argv = raw.get("argv")
        if (
            not isinstance(argv, list)
            or not argv
            or len(argv) > 64
            or any(not isinstance(arg, str) or not arg or len(arg) > 4096 for arg in argv)
        ):
            fail("spec check %s has invalid argv" % check_id)
        required = raw.get("required", True)
        if not isinstance(required, bool):
            fail("spec check %s required must be boolean" % check_id)
        timeout = raw.get("timeout_s", 60)
        if isinstance(timeout, bool) or not isinstance(timeout, int) or not 1 <= timeout <= 600:
            fail("spec check %s timeout_s must be 1..600" % check_id)
        cwd = raw.get("cwd", ".")
        if not isinstance(cwd, str) or not cwd or len(cwd) > 1024:
            fail("spec check %s cwd is invalid" % check_id)
        checks.append(
            {
                "id": check_id,
                "argv": argv,
                "required": required,
                "timeout_s": timeout,
                "cwd": cwd,
            }
        )

    raw_rubric = spec.get("rubric", [])
    if not isinstance(raw_rubric, list) or len(raw_rubric) > 50:
        fail("spec.rubric must be a list of at most 50 dimensions")
    rubric = []
    rubric_ids = set()
    for index, raw in enumerate(raw_rubric):
        if not isinstance(raw, dict):
            fail("spec.rubric[%d] must be an object" % index)
        rubric_id = raw.get("id")
        if not isinstance(rubric_id, str) or not SAFE_ID.fullmatch(rubric_id):
            fail("spec.rubric[%d].id is invalid" % index)
        if rubric_id in rubric_ids:
            fail("spec rubric ids must be unique")
        rubric_ids.add(rubric_id)
        weight = bounded_number(raw.get("weight", 1.0), "rubric weight", 0.000001, 1000)
        rubric.append({"id": rubric_id, "weight": weight})

    threshold = bounded_number(spec.get("threshold", 0.8), "spec.threshold", 0, 1)
    independent = spec.get("require_independent_judge", True)
    if not isinstance(independent, bool):
        fail("spec.require_independent_judge must be boolean")
    return {
        "id": eval_id,
        "checks": checks,
        "rubric": rubric,
        "threshold": threshold,
        "require_independent_judge": independent,
    }


def read_subject(repo: Path, event_id: str) -> Dict[str, Any]:
    if not SAFE_ID.fullmatch(event_id):
        fail("--subject-event is invalid")
    path = repo / ".oms" / "artifacts" / "index.jsonl"
    matches = []
    try:
        handle = path.open(encoding="utf-8", errors="replace")
    except OSError as exc:
        fail("cannot read artifact index: %s" % exc)
    with handle:
        for raw in handle:
            if len(raw) > 1024 * 1024:
                continue
            try:
                row = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if isinstance(row, dict) and row.get("event_id") == event_id:
                matches.append(row)
    if not matches:
        fail("subject event was not found")
    if len(matches) != 1:
        fail("subject event is not unique")
    subject = matches[0]
    if subject.get("schema") != 1:
        fail("subject event has an unsupported schema")
    return subject


def safe_patch(repo: Path, subject: Dict[str, Any]) -> Tuple[str, bytes, str]:
    base = subject.get("base_sha")
    raw_patch = subject.get("patch")
    if not isinstance(base, str) or not re.fullmatch(r"[0-9a-fA-F]{7,64}", base):
        fail("subject event needs a valid base_sha for deterministic checks")
    if not isinstance(raw_patch, str) or not raw_patch or len(raw_patch) > 1024:
        fail("subject event needs a relative patch path for deterministic checks")
    relative = Path(raw_patch)
    if relative.is_absolute():
        fail("subject patch must be inside --repo")
    try:
        patch = (repo / relative).resolve(strict=True)
        patch.relative_to(repo)
    except (OSError, ValueError):
        fail("subject patch must be an existing file inside --repo")
    if not patch.is_file():
        fail("subject patch is not a regular file")
    expected = subject.get("patch_sha256")
    patch_bytes = read_once(patch, "subject patch")
    actual = hashlib.sha256(patch_bytes).hexdigest()
    if expected is not None and expected != actual:
        fail("subject patch checksum does not match the artifact row")
    return base, patch_bytes, actual


def git(repo: Path, args: List[str], timeout: int = 30) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            ["git", "-C", str(repo)] + args,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("git operation failed: %s" % exc)


def run_check(tree: Path, check: Dict[str, Any]) -> Dict[str, Any]:
    raw_cwd = Path(check["cwd"])
    if raw_cwd.is_absolute():
        fail("check %s cwd must be relative" % check["id"])
    try:
        cwd = (tree / raw_cwd).resolve(strict=True)
        cwd.relative_to(tree)
    except (OSError, ValueError):
        fail("check %s cwd must be a directory inside the worktree" % check["id"])
    if not cwd.is_dir():
        fail("check %s cwd is not a directory" % check["id"])

    def stop_group(process: subprocess.Popen[Any]) -> None:
        if os.name == "posix":
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                return
            try:
                process.wait(timeout=0.25)
            except subprocess.TimeoutExpired:
                pass
            # Waiting for the group leader is insufficient: a check can fork,
            # exit, and leave descendants behind. Kill the still-owned session
            # before returning even when the leader has already been reaped.
            try:
                os.killpg(process.pid, 0)
            except (ProcessLookupError, PermissionError):
                return
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
            return
        command = ["taskkill", "/PID", str(process.pid), "/T"]
        try:
            subprocess.run(
                command,
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=5,
            )
        except (OSError, subprocess.TimeoutExpired):
            try:
                process.terminate()
            except OSError:
                return
        try:
            process.wait(timeout=0.25)
        except subprocess.TimeoutExpired:
            try:
                subprocess.run(
                    command + ["/F"],
                    check=False,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=5,
                )
            except (OSError, subprocess.TimeoutExpired):
                try:
                    process.kill()
                except OSError:
                    pass

    started = time.monotonic()
    timed_out = False
    output_limited_event = threading.Event()
    exit_code = 127
    output_hash = hashlib.sha256()
    output_bytes = 0
    process: Optional[subprocess.Popen[Any]] = None
    reader: Optional[threading.Thread] = None

    def drain_output(stream: Any) -> None:
        nonlocal output_bytes
        while True:
            chunk = stream.read(65536)
            if not chunk:
                return
            remaining = max(0, MAX_CHECK_OUTPUT_BYTES - output_bytes)
            if remaining:
                output_hash.update(chunk[:remaining])
            output_bytes += len(chunk)
            if output_bytes > MAX_CHECK_OUTPUT_BYTES:
                output_limited_event.set()

    try:
        process = subprocess.Popen(
            check["argv"],
            cwd=str(cwd),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            start_new_session=(os.name == "posix"),
            creationflags=(subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0),
        )
        if process.stdout is None:
            raise OSError("check output pipe was not created")
        reader = threading.Thread(target=drain_output, args=(process.stdout,), daemon=True)
        reader.start()
        deadline = started + check["timeout_s"]
        while process.poll() is None:
            if output_limited_event.is_set():
                stop_group(process)
                break
            if time.monotonic() >= deadline:
                timed_out = True
                stop_group(process)
                break
            time.sleep(0.02)
        # A command may background descendants and exit. Checks never retain
        # those processes after their bounded parent finishes.
        stop_group(process)
        if reader is not None:
            reader.join(timeout=3)
            if reader.is_alive():
                try:
                    process.stdout.close()
                except OSError:
                    pass
                reader.join(timeout=1)
        if timed_out:
            exit_code = 124
        elif output_limited_event.is_set():
            exit_code = 125
        else:
            exit_code = int(process.wait(timeout=3))
            if exit_code < 0:
                exit_code = 128 + (-exit_code)
    except (OSError, subprocess.SubprocessError) as exc:
        encoded = str(exc).encode("utf-8", errors="replace")[:MAX_CHECK_OUTPUT_BYTES]
        output_hash.update(encoded)
        exit_code = 127
        if process is not None:
            stop_group(process)
    duration = round(time.monotonic() - started, 3)
    return {
        "id": check["id"],
        "required": check["required"],
        "exit": exit_code,
        "timed_out": timed_out,
        "output_limited": output_limited_event.is_set(),
        "duration_s": duration,
        "output_sha256": output_hash.hexdigest(),
    }


def evaluate_checks(
    repo: Path, subject: Dict[str, Any], checks: List[Dict[str, Any]]
) -> Tuple[List[Dict[str, Any]], List[str]]:
    if not checks:
        return [], []
    base, patch_bytes, _ = safe_patch(repo, subject)
    temp_root = Path(tempfile.mkdtemp(prefix="oms-semantic-eval-"))
    tree = temp_root / "worktree"
    added = False
    results: List[Dict[str, Any]] = []
    reasons: List[str] = []
    try:
        frozen_patch = temp_root / "subject.patch"
        frozen_patch.write_bytes(patch_bytes)
        os.chmod(str(frozen_patch), 0o600)
        add = git(repo, ["worktree", "add", "--detach", str(tree), base], timeout=60)
        if add.returncode != 0:
            fail("cannot create isolated worktree for subject base")
        added = True
        check_patch = git(tree, ["apply", "--check", str(frozen_patch)])
        if check_patch.returncode != 0:
            reasons.append("subject patch does not apply to its recorded base")
            return results, reasons
        apply_patch = git(tree, ["apply", str(frozen_patch)])
        if apply_patch.returncode != 0:
            reasons.append("subject patch could not be applied in the isolated worktree")
            return results, reasons
        for check in checks:
            results.append(run_check(tree, check))
        return results, reasons
    finally:
        if added:
            git(repo, ["worktree", "remove", "--force", str(tree)], timeout=60)
            git(repo, ["worktree", "prune"], timeout=30)
        shutil.rmtree(str(temp_root), ignore_errors=True)


def provider_family(value: Any) -> str:
    provider = str(value or "").strip().lower()
    if provider in ("codex", "openai"):
        return "openai"
    if provider in ("claude", "anthropic"):
        return "anthropic"
    if provider in ("antigravity", "agy", "gemini", "google"):
        return "google"
    return provider


def evaluate_judge(
    spec: Dict[str, Any], subject: Dict[str, Any], judge_path: Optional[Path]
) -> Tuple[Optional[Dict[str, Any]], Optional[float], List[str]]:
    rubric = spec["rubric"]
    if not rubric:
        return None, 1.0, []
    if judge_path is None:
        return None, None, ["semantic judge result is required"]
    try:
        with judge_path.open("rb") as handle:
            judge_bytes = handle.read(MAX_INPUT_BYTES + 1)
    except OSError:
        return None, None, ["semantic judge result could not be read"]
    if len(judge_bytes) > MAX_INPUT_BYTES:
        return None, None, ["semantic judge result exceeds the input limit"]
    try:
        judge = json.loads(judge_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None, None, ["semantic judge result is invalid JSON"]
    if not isinstance(judge, dict):
        return None, None, ["semantic judge result must be an object"]
    reasons = []
    if judge.get("schema") != 1:
        return None, None, ["semantic judge result has an unsupported schema"]
    provider = judge.get("provider")
    model = judge.get("model")
    verdict = judge.get("verdict")
    scores = judge.get("scores")
    if not isinstance(provider, str) or not provider or len(provider) > 80:
        return None, None, ["semantic judge provider is invalid"]
    if not isinstance(model, str) or not model or len(model) > 160:
        return None, None, ["semantic judge model is invalid"]
    if verdict not in ("pass", "fail"):
        return None, None, ["semantic judge verdict must be pass or fail"]
    if not isinstance(scores, dict):
        return None, None, ["semantic judge scores are missing"]

    bounded_scores = {}
    weighted = 0.0
    total_weight = 0.0
    for dimension in rubric:
        rubric_id = dimension["id"]
        value = scores.get(rubric_id)
        if (
            isinstance(value, bool)
            or not isinstance(value, (int, float))
            or float(value) != float(value)
            or not 0 <= float(value) <= 1
        ):
            return None, None, ["semantic judge score %s is invalid" % rubric_id]
        score = float(value)
        bounded_scores[rubric_id] = score
        weighted += score * dimension["weight"]
        total_weight += dimension["weight"]
    score = round(weighted / total_weight, 3)

    if spec["require_independent_judge"]:
        # A local JSON file has no authenticated execution provenance. Labels
        # in it are useful metadata, but cannot prove independent execution.
        reasons.append(
            "independent judge provenance is unverified; provider and model are self-reported"
        )
        subject_family = provider_family(subject.get("provider"))
        judge_family = provider_family(provider)
        if not subject_family or subject_family == judge_family:
            reasons.append(
                "independent judge required: subject and judge use the same provider family"
            )
    bounded = {
        "provider": provider,
        "model": model,
        "verdict": verdict,
        "scores": bounded_scores,
        "provenance": "self-reported",
    }
    return bounded, score, reasons


def iso_now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def write_result(path: str, report: Dict[str, Any], force: bool) -> None:
    encoded = json.dumps(report, ensure_ascii=False, sort_keys=True) + "\n"
    if path == "-":
        sys.stdout.write(encoded)
        return
    target = Path(path).resolve()
    if not target.parent.is_dir():
        fail("output parent directory does not exist")
    temp_name = ""
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            prefix=".%s." % target.name,
            dir=str(target.parent),
            delete=False,
        ) as handle:
            temp_name = handle.name
            os.chmod(temp_name, 0o600)
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        if force:
            os.replace(temp_name, str(target))
            temp_name = ""
        else:
            try:
                os.link(temp_name, str(target))
            except FileExistsError:
                os.unlink(temp_name)
                temp_name = ""
                fail("output exists; pass --force to replace it")
            os.unlink(temp_name)
            temp_name = ""
    except OSError as exc:
        if temp_name:
            try:
                os.unlink(temp_name)
            except OSError:
                pass
        fail("cannot write output: %s" % exc)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Evaluate a recorded patch in isolation with checks and a bounded rubric."
    )
    parser.add_argument("--repo", default=".", help="repository containing the recorded patch")
    parser.add_argument("--spec", required=True, help="trusted evaluation spec JSON")
    parser.add_argument("--subject-event", required=True, help="unique artifact event identifier")
    parser.add_argument("--judge-result", help="self-reported rubric result JSON; no model is invoked")
    parser.add_argument(
        "--allow-host-checks",
        action="store_true",
        help="run trusted spec argv on this host in a temporary git worktree",
    )
    parser.add_argument("--output", default="-", help="result JSON file, or - for stdout")
    parser.add_argument("--force", action="store_true", help="replace an existing output file")
    args = parser.parse_args()
    if args.force and args.output == "-":
        fail("--force is only valid with --output FILE")

    started_at = iso_now()
    started = time.monotonic()
    repo = repo_root(args.repo)
    spec_path = Path(args.spec).resolve()
    spec_bytes = read_once(spec_path, "spec")
    raw_spec = load_object_bytes(spec_bytes, "spec")
    spec = validate_spec(raw_spec)
    if spec["checks"] and not args.allow_host_checks:
        fail(
            "spec checks execute commands on the host; review the trusted spec and pass --allow-host-checks"
        )
    subject = read_subject(repo, args.subject_event)

    checks, reasons = evaluate_checks(repo, subject, spec["checks"])
    for result in checks:
        if result["required"] and result["exit"] != 0:
            reasons.append("required check %s failed" % result["id"])
    deterministic_failure = bool(reasons)

    judge_path = Path(args.judge_result).resolve() if args.judge_result else None
    judge, score, judge_reasons = evaluate_judge(spec, subject, judge_path)
    reasons.extend(judge_reasons)
    incomplete = bool(judge_reasons)
    judge_failure = bool(
        judge is not None
        and (judge["verdict"] == "fail" or (score is not None and score < spec["threshold"]))
    )
    if judge is not None and judge["verdict"] == "fail":
        reasons.append("semantic judge returned fail")
    elif judge is not None and score is not None and score < spec["threshold"]:
        reasons.append("semantic score is below threshold")
    if deterministic_failure or judge_failure:
        outcome = "fail"
    elif incomplete:
        outcome = "incomplete"
    else:
        outcome = "pass"

    subject_view = {
        "event_id": subject.get("event_id"),
        "artifact_id": subject.get("artifact_id"),
        "provider": subject.get("provider"),
        "kind": subject.get("kind"),
        "base_sha": subject.get("base_sha"),
        "patch_sha256": subject.get("patch_sha256"),
    }
    report = {
        "schema": 1,
        "action": "semantic-eval",
        "eval_id": spec["id"],
        "spec_sha256": hashlib.sha256(spec_bytes).hexdigest(),
        "started_at": started_at,
        "duration_s": round(time.monotonic() - started, 3),
        "subject": subject_view,
        "checks": checks,
        "host_checks": {
            "authorized": bool(args.allow_host_checks),
            "profile": "trusted-local",
            "sandboxed": False,
        },
        "judge": judge,
        "score": score,
        "threshold": spec["threshold"],
        "semantic_outcome": outcome,
        "reasons": reasons,
    }
    write_result(args.output, report, args.force)
    return {"pass": 0, "fail": 1, "incomplete": 3}[outcome]


if __name__ == "__main__":
    raise SystemExit(main())
