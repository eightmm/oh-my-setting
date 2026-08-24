#!/usr/bin/env python3
"""Exact, crash-recoverable retirement of the canonical active plan.

The public surface and lock orchestration live in agent-plan.sh. This module
owns only the byte/generation CAS, archive/receipt transaction, and the exact
acceptance context predicate reused immediately before and after acceptance.
"""

import collections
import errno
import hashlib
import json
import math
import os
import re
import secrets
import stat
import subprocess
import sys


CONTEXT = {}
STATES = set()
ID_RE = None
KNOWN_STATES = {"ready", "claimed", "running", "review", "landing", "blocked", "done"}
PLAN_ID_RE = re.compile(r"plan_[0-9a-f]{32}")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9._-]+$")
MAX_JSONL_ROW = 1024 * 1024


def env(name):
    return os.environ.get(name, "")


def die(message):
    callback = CONTEXT.get("die")
    if callback:
        callback(message)
    sys.stderr.write("error: %s\n" % message)
    raise SystemExit(2)


def same_absolute_path(left, right):
    return os.path.normcase(os.path.abspath(left)) == os.path.normcase(os.path.abspath(right))


def is_reparse(info):
    attribute = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(info, "st_file_attributes", 0) & attribute)


def checked_plan_parent(repo_root):
    expected_parent = os.path.join(os.path.realpath(repo_root), ".oms", "plan")
    try:
        info = os.lstat(expected_parent)
    except OSError as exc:
        die("cannot inspect plan authority directory: %s" % exc)
    if (not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or
            is_reparse(info) or
            not same_absolute_path(os.path.realpath(expected_parent), expected_parent)):
        die("plan authority directory must be the real repo-local .oms/plan directory")
    return expected_parent


def retirement_parent():
    repo_root = os.path.realpath(env("OMS_REPO"))
    expected_parent = checked_plan_parent(repo_root)
    expected_plan = os.path.join(expected_parent, "tasks.json")
    if not same_absolute_path(CONTEXT["path"], expected_plan):
        die("retire is valid only for the canonical repo-local plan")
    return repo_root, expected_parent


def file_generation(info):
    return {
        "dev": int(info.st_dev),
        "ino": int(info.st_ino),
        "size": int(info.st_size),
        "mtime_ns": int(getattr(info, "st_mtime_ns", int(info.st_mtime * 1000000000))),
        "ctime_ns": int(getattr(info, "st_ctime_ns", int(info.st_ctime * 1000000000))),
    }


def valid_generation(value):
    keys = {"dev", "ino", "size", "mtime_ns", "ctime_ns"}
    return (isinstance(value, dict) and set(value) == keys and
            all(isinstance(value.get(key), int) and
                not isinstance(value.get(key), bool) and value.get(key) >= 0
                for key in keys))


def bounded_regular(filename, label, maximum, absent_ok=False):
    try:
        before = os.lstat(filename)
    except FileNotFoundError:
        if absent_ok:
            return None, None
        die("%s is missing" % label)
    except OSError as exc:
        die("cannot inspect %s: %s" % (label, exc))
    if (stat.S_ISLNK(before.st_mode) or is_reparse(before) or
            not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or
            before.st_size > maximum):
        die("%s must be one bounded regular non-link file" % label)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(filename, flags)
    except OSError as exc:
        die("cannot open %s safely: %s" % (label, exc))
    try:
        opened = os.fstat(descriptor)
        if (not stat.S_ISREG(opened.st_mode) or is_reparse(opened) or
                opened.st_nlink != 1 or
                (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
            die("%s changed while opening" % label)
        chunks = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > maximum:
                die("%s exceeds %d bytes" % (label, maximum))
    finally:
        os.close(descriptor)
    try:
        after = os.lstat(filename)
    except OSError:
        die("%s changed while reading" % label)
    if ((after.st_dev, after.st_ino, after.st_size) !=
            (opened.st_dev, opened.st_ino, opened.st_size)):
        die("%s changed while reading" % label)
    return b"".join(chunks), file_generation(opened)


def fsync_directory(directory):
    if os.name == "nt":
        return
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    descriptor = os.open(directory, flags)
    try:
        try:
            os.fsync(descriptor)
        except OSError as exc:
            if exc.errno not in {errno.EINVAL, errno.ENOTSUP, errno.EBADF}:
                raise
    finally:
        os.close(descriptor)


def plan_from_payload(payload):
    try:
        value = json.loads(payload.decode("utf-8"))
    except (UnicodeError, ValueError):
        die("plan is not valid UTF-8 JSON")
    if not isinstance(value, dict) or not isinstance(value.get("tasks", {}), dict):
        die("plan must contain an object task map")
    plan_id = value.get("plan_id", "")
    if (not isinstance(plan_id, str) or
            (plan_id and not PLAN_ID_RE.fullmatch(plan_id))):
        die("plan has an invalid plan_id")
    task_rows = value.get("tasks", {})
    task_id_re = ID_RE or TASK_ID_RE
    allowed_states = STATES or KNOWN_STATES
    for task_id, task in task_rows.items():
        if not isinstance(task_id, str) or not task_id_re.fullmatch(task_id):
            die("plan contains an invalid task id")
        if not isinstance(task, dict) or task.get("state", "ready") not in allowed_states:
            die("plan task %s has an invalid state" % task_id)
    return value, task_rows


def git_snapshot(repo_root):
    git_env = os.environ.copy()
    git_env["GIT_CONFIG_GLOBAL"] = os.devnull
    git_env["GIT_CONFIG_SYSTEM"] = os.devnull
    git_env["GIT_OPTIONAL_LOCKS"] = "0"
    try:
        head = subprocess.check_output(
            ["git", "-C", repo_root, "rev-parse", "HEAD"],
            stderr=subprocess.DEVNULL, env=git_env,
        ).decode("ascii").strip().replace("\r", "")
        symbolic = subprocess.run(
            ["git", "-C", repo_root, "symbolic-ref", "-q", "HEAD"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=git_env,
            check=False,
        )
        if symbolic.returncode == 0:
            ref = symbolic.stdout.decode("utf-8").strip().replace("\r", "")
        elif symbolic.returncode == 1:
            ref = "DETACHED@" + head
        else:
            die("cannot resolve repository symbolic ref")
        dirty = subprocess.check_output(
            ["git", "-c", "core.fsmonitor=false", "-C", repo_root, "status",
             "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none"],
            stderr=subprocess.DEVNULL, env=git_env,
        )
    except (OSError, subprocess.CalledProcessError, UnicodeError):
        die("cannot freeze the repository for plan retirement")
    return head, ref, not bool(dirty)


def reject_json_constant(value):
    raise ValueError("non-finite JSON value: %s" % value)


def reject_duplicate_keys(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON key: %s" % key)
        value[key] = item
    return value


def parse_jsonl_row(line, number, label):
    if len(line) > MAX_JSONL_ROW:
        die("%s row %d exceeds 1048576 bytes" % (label, number))
    payload = line[:-1]
    if payload.endswith(b"\r"):
        payload = payload[:-1]
    if b"\r" in payload:
        die("%s row %d contains a bare CR" % (label, number))
    if not payload.strip():
        die("%s row %d is blank" % (label, number))
    if b"\x00" in payload:
        die("%s row %d contains a NUL byte" % (label, number))
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as exc:
        die("%s row %d is invalid UTF-8: %s" % (label, number, exc))
    try:
        row = json.loads(
            text, parse_constant=reject_json_constant,
            object_pairs_hook=reject_duplicate_keys,
        )
    except ValueError as exc:
        die("%s row %d is invalid JSON: %s" % (label, number, exc))
    if not isinstance(row, dict):
        die("%s row %d is not an object" % (label, number))
    def finite(value):
        if isinstance(value, float):
            return math.isfinite(value)
        if isinstance(value, dict):
            return all(finite(item) for item in value.values())
        if isinstance(value, list):
            return all(finite(item) for item in value)
        return True
    if not finite(row):
        die("%s row %d contains a non-finite JSON number" % (label, number))
    return row


def read_jsonl_checked(filename, label):
    payload, _ = bounded_regular(filename, label, 16 * 1024 * 1024, absent_ok=True)
    if payload is None:
        return []
    if not payload:
        return []
    if not payload.endswith(b"\n"):
        die("%s lacks its terminal LF" % label)
    lines = [raw + b"\n" for raw in payload[:-1].split(b"\n")]
    return [parse_jsonl_row(line, number, label)
            for number, line in enumerate(lines, 1)]


def parse_single_jsonl_object(payload, label):
    if not payload.endswith(b"\n"):
        die("%s lacks its terminal LF" % label)
    if b"\n" in payload[:-1]:
        die("%s must contain exactly one JSON object row" % label)
    return parse_jsonl_row(payload, 1, label)


def read_single_jsonl_object(filename, label, maximum=64 * 1024):
    payload, _ = bounded_regular(filename, label, maximum, absent_ok=False)
    return parse_single_jsonl_object(payload, label)


def link_durable(temporary, target, parent, occupied):
    try:
        os.link(temporary, target)
    except FileExistsError:
        occupied()
    # Remove the construction name before the directory durability barrier.
    # If the process dies between link/unlink, the next apply recognizes only
    # this deterministic same-inode pair and finishes it.
    os.unlink(temporary)
    fsync_directory(parent)


def recover_link_pair(target, temporary, parent, label):
    if not (os.path.lexists(target) and os.path.lexists(temporary)):
        return
    try:
        target_info = os.lstat(target)
        temporary_info = os.lstat(temporary)
    except OSError as exc:
        die("cannot inspect interrupted %s link: %s" % (label, exc))
    if (stat.S_ISLNK(target_info.st_mode) or stat.S_ISLNK(temporary_info.st_mode) or
            is_reparse(target_info) or is_reparse(temporary_info) or
            not stat.S_ISREG(target_info.st_mode) or
            not stat.S_ISREG(temporary_info.st_mode) or
            (target_info.st_dev, target_info.st_ino) !=
            (temporary_info.st_dev, temporary_info.st_ino) or
            target_info.st_nlink != 2 or temporary_info.st_nlink != 2):
        die("interrupted %s link pair is unsafe" % label)
    os.unlink(temporary)
    fsync_directory(parent)


def atomic_intent(filename, row, parent):
    temporary = os.path.join(parent, ".%s.tmp" % os.path.basename(filename))
    if os.path.lexists(temporary):
        try:
            info = os.lstat(temporary)
        except OSError as exc:
            die("cannot inspect retirement intent temporary: %s" % exc)
        if (stat.S_ISLNK(info.st_mode) or is_reparse(info) or
                not stat.S_ISREG(info.st_mode) or info.st_nlink != 1):
            die("retirement intent temporary path is unsafe")
        os.unlink(temporary)
        fsync_directory(parent)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(temporary, flags, 0o600)
    try:
        payload = (json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise OSError("short intent write")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    try:
        link_durable(temporary, filename, parent,
                     lambda: die("retirement intent path became occupied"))
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def durable_archive(filename, payload, proof_id, parent):
    temporary = os.path.join(parent, ".%s.tmp" % os.path.basename(filename))
    recover_link_pair(filename, temporary, parent, "retirement archive")
    existing, _ = bounded_regular(filename, "retirement archive", 1024 * 1024,
                                  absent_ok=True)
    if existing is not None:
        if existing != payload:
            die("retirement archive path is occupied by different bytes")
        return
    if os.path.lexists(temporary):
        info = os.lstat(temporary)
        if (stat.S_ISLNK(info.st_mode) or is_reparse(info) or
                not stat.S_ISREG(info.st_mode) or info.st_nlink != 1):
            die("retirement archive temporary path is unsafe")
        os.unlink(temporary)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(temporary, flags, 0o600)
    try:
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise OSError("short archive write")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)

    def occupied():
        value, _ = bounded_regular(
            filename, "retirement archive", 1024 * 1024, absent_ok=False)
        if value != payload:
            die("retirement archive path became occupied by different bytes")

    try:
        link_durable(temporary, filename, parent, occupied)
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def append_receipt(filename, row):
    completed = subprocess.run(
        [sys.executable, CONTEXT["durable_jsonl"], "--label", "retirements.jsonl",
         "append", filename],
        input=json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n",
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    if completed.returncode != 0:
        die("cannot durably append plan retirement receipt: %s" %
            completed.stderr.strip()[:300])


def paths(parent, plan_sha):
    return (
        os.path.join(parent, "tasks.%s.archive.json" % plan_sha),
        os.path.join(parent, "tasks.%s.retire-intent.json" % plan_sha),
        os.path.join(parent, "retirements.jsonl"),
    )


def validate_authority(tasks):
    active = sorted(task_id for task_id, task in tasks.items()
                    if task.get("state", "ready") in
                    {"claimed", "running", "review", "landing"})
    if active:
        die("active plan task authority prevents retirement: %s" % ", ".join(active))
    markers = CONTEXT["load_worker_markers"]()
    for marker in markers:
        if not CONTEXT["worker_marker_is_typed"](marker):
            die("worker marker set is malformed or unproven; refusing retirement")
        if marker.get("task_id") in tasks and CONTEXT["marker_pid_alive"](marker):
            die("live worker marker prevents retirement for task %s" % marker.get("task_id"))


def matching_receipt(rows, plan_sha):
    matched = [row for row in rows
               if row.get("kind") == "plan-retirement" and
               row.get("plan_sha256") == plan_sha]
    if len(matched) > 1:
        die("multiple retirement receipts claim the same plan bytes")
    return matched[0] if matched else None


def validate_receipt(row, repo_root, archive, plan_sha, disposition, reason):
    archive_rel = os.path.relpath(archive, repo_root).replace(os.sep, "/")
    if (row.get("plan_sha256") != plan_sha or
            row.get("disposition") != disposition or row.get("reason") != reason or
            row.get("archive") != archive_rel):
        die("existing plan retirement receipt is malformed or conflicts with this operation")
    return validate_receipt_authority(row, repo_root)


def acceptance_proof(parent, expected):
    rows = read_jsonl_checked(os.path.join(parent, "progress.jsonl"),
                              "plan acceptance progress")
    matched = [row for row in rows
               if row.get("kind") == "acceptance" and
               row.get("retire_proof_id") == expected["proof_id"]]
    if len(matched) != 1:
        die("completed-external requires the fresh acceptance run from this retire call")
    row = matched[0]
    if (row.get("schema") != 1 or
            not isinstance(row.get("ts"), str) or not row.get("ts") or
            row.get("status") != "pass" or row.get("exit") != 0 or
            isinstance(row.get("exit"), bool) or row.get("reason") != "" or
            row.get("timed_out") is not False or
            row.get("output_limited") is not False or row.get("git_clean") is not True or
            row.get("plan_sha256") != expected["plan_sha256"] or
            row.get("base_sha") != expected["head"] or
            row.get("git_ref") != expected["git_ref"] or
            row.get("plan_generation") != expected["plan_generation"] or
            row.get("accept_sha256_full") != expected["accept_sha256"] or
            not re.fullmatch(r"[0-9a-f]{64}", str(row.get("output_sha256_full", ""))) or
            not re.fullmatch(r"[0-9a-f]{64}", str(row.get("repo_snapshot_sha256", ""))) or
            row.get("accept_sha256") != row.get("accept_sha256_full", "")[:16] or
            row.get("output_sha256") != row.get("output_sha256_full", "")[:16]):
        die("completed-external acceptance proof is stale or belongs to another retire call")
    return {
        "ts": row.get("ts", ""),
        "base_sha": row.get("base_sha", ""),
        "git_ref": row.get("git_ref", ""),
        "accept_sha256": row.get("accept_sha256_full", ""),
        "output_sha256": row.get("output_sha256_full", ""),
        "repo_snapshot_sha256": row.get("repo_snapshot_sha256", ""),
        "plan_sha256": row.get("plan_sha256", ""),
        "proof_id": row.get("retire_proof_id", ""),
    }


def validate_receipt_authority(row, repo_root):
    """Validate one retirement receipt against its archive and proof ledger."""
    if not isinstance(row, dict):
        die("plan retirement receipt is not an object")
    disposition = row.get("disposition")
    required = {
        "schema", "kind", "ts", "retirement_id", "plan_sha256", "plan_id",
        "disposition", "reason", "head", "git_ref", "archive",
        "plan_generation", "proof_id", "acceptance_verified",
        "task_landings_claimed", "task_states",
    }
    if disposition == "completed-external":
        required.add("acceptance")
    if set(row) != required:
        die("plan retirement receipt has unexpected or missing fields")
    plan_sha = row.get("plan_sha256")
    proof_id = row.get("proof_id")
    generation = row.get("plan_generation")
    plan_id = row.get("plan_id")
    task_states = row.get("task_states")
    expected_archive = ".oms/plan/tasks.%s.archive.json" % plan_sha
    if (row.get("schema") != 1 or row.get("kind") != "plan-retirement" or
            not isinstance(row.get("ts"), str) or not row.get("ts") or
            not isinstance(plan_sha, str) or not re.fullmatch(r"[0-9a-f]{64}", plan_sha) or
            not isinstance(proof_id, str) or not re.fullmatch(r"[0-9a-f]{64}", proof_id) or
            row.get("retirement_id") != "retire_" + proof_id or
            disposition not in {"completed-external", "superseded"} or
            not isinstance(row.get("reason"), str) or not 0 < len(row.get("reason")) <= 500 or
            not isinstance(row.get("head"), str) or
            not re.fullmatch(r"[0-9a-f]{40,64}", row.get("head")) or
            not isinstance(row.get("git_ref"), str) or not row.get("git_ref") or
            not valid_generation(generation) or
            not isinstance(plan_id, str) or
            (plan_id and not PLAN_ID_RE.fullmatch(plan_id)) or
            not isinstance(task_states, dict) or
            any(state not in KNOWN_STATES or not isinstance(count, int) or
                isinstance(count, bool) or count < 0
                for state, count in task_states.items()) or
            row.get("archive") != expected_archive or
            row.get("task_landings_claimed") is not False):
        die("plan retirement receipt has invalid core fields")

    parent = checked_plan_parent(repo_root)
    archive = os.path.join(parent, "tasks.%s.archive.json" % plan_sha)
    archived, _ = bounded_regular(archive, "retirement archive", 1024 * 1024,
                                  absent_ok=True)
    if archived is None or hashlib.sha256(archived).hexdigest() != plan_sha:
        die("retirement receipt does not have its exact content-addressed archive")
    plan, tasks = plan_from_payload(archived)
    actual_states = dict(sorted(collections.Counter(
        task.get("state", "ready") for task in tasks.values()).items()))
    if actual_states != task_states or plan.get("plan_id", "") != plan_id:
        die("retirement receipt task states or plan_id contradict its archive")
    if any(state in actual_states for state in {"claimed", "running", "review", "landing"}):
        die("retirement archive contains active task authority")

    if disposition == "completed-external":
        accept = plan.get("accept", "")
        if not isinstance(accept, str) or not accept:
            die("completed-external archive lacks an acceptance command")
        snapshot = {
            "plan_sha256": plan_sha,
            "head": row.get("head"),
            "git_ref": row.get("git_ref"),
            "plan_generation": generation,
            "proof_id": proof_id,
            "accept_sha256": hashlib.sha256(accept.encode("utf-8")).hexdigest(),
        }
        proof = acceptance_proof(parent, snapshot)
        if row.get("acceptance_verified") is not True or row.get("acceptance") != proof:
            die("completed-external retirement receipt lacks its exact fresh acceptance proof")
    elif (row.get("acceptance_verified") is not False or
            any(state != "done" for state in actual_states)):
        die("superseded retirement receipt contradicts its all-done archive")
    return row


def validate_retirement_ledger(repo_root, filename=None):
    parent = checked_plan_parent(repo_root)
    expected = os.path.join(parent, "retirements.jsonl")
    if filename is not None and not same_absolute_path(filename, expected):
        die("retirement ledger must be the canonical repo-local authority file")
    rows = read_jsonl_checked(expected, "plan retirement receipts")
    seen_plans = set()
    seen_proofs = set()
    for row in rows:
        validate_receipt_authority(row, repo_root)
        plan_sha = row["plan_sha256"]
        proof_id = row["proof_id"]
        if plan_sha in seen_plans:
            die("plan %s has duplicate retirement receipts" % plan_sha[:12])
        if proof_id in seen_proofs:
            die("multiple retirement receipts reuse one proof token")
        seen_plans.add(plan_sha)
        seen_proofs.add(proof_id)
    return rows


def intent_from_receipt(row):
    return {
        "schema": 1, "kind": "plan-retirement-intent",
        "plan_sha256": row.get("plan_sha256"),
        "disposition": row.get("disposition"), "reason": row.get("reason"),
        "head": row.get("head"), "git_ref": row.get("git_ref"),
        "plan_generation": row.get("plan_generation"),
        "proof_id": row.get("proof_id"), "archive": row.get("archive"),
    }


def residual_retirement_intents(parent):
    matches = []
    for entry in os.scandir(parent):
        match = re.fullmatch(
            r"tasks\.([0-9a-f]{64})\.retire-intent\.json", entry.name)
        if match:
            matches.append((match.group(1), entry.path))
    return sorted(matches)


def cleanup_completed_retirement(context):
    """Remove one validated post-unlink intent before a topology write."""
    global CONTEXT, STATES, ID_RE
    CONTEXT = context
    STATES = context["states"]
    ID_RE = context["id_re"]
    repo_root = os.path.realpath(env("OMS_REPO"))
    parent = checked_plan_parent(repo_root)
    matches = residual_retirement_intents(parent)
    if not matches:
        return False
    if len(matches) != 1:
        die("multiple completed retirement intents require exact manual replay")
    plan_sha, intent_path = matches[0]
    rows = validate_retirement_ledger(repo_root)
    prior = matching_receipt(rows, plan_sha)
    if prior is None:
        die("retirement intent lacks its validated durable receipt")
    intent = read_single_jsonl_object(intent_path, "plan retirement intent")
    if intent != intent_from_receipt(prior):
        die("completed retirement has a conflicting residual intent")
    active_payload, active_generation = bounded_regular(
        context["path"], "active plan", 1024 * 1024, absent_ok=True)
    if active_payload is not None and active_generation == prior.get("plan_generation"):
        die("exact retired plan generation still requires retire replay")
    if active_payload is not None:
        active_plan, _ = plan_from_payload(active_payload)
        active_plan_id = active_plan.get("plan_id", "")
        if not active_plan_id or active_plan_id == prior.get("plan_id"):
            die("active plan lineage still requires exact retire replay")
    os.unlink(intent_path)
    fsync_directory(parent)
    return True


def validate_active_retirement_lineage(repo_root, rows):
    parent = checked_plan_parent(repo_root)
    active = os.path.join(parent, "tasks.json")
    payload, _ = bounded_regular(active, "active plan", 1024 * 1024, absent_ok=True)
    if payload is None:
        return
    plan, _ = plan_from_payload(payload)
    active_id = plan.get("plan_id", "")
    active_sha = hashlib.sha256(payload).hexdigest()
    for row in rows:
        if (active_sha == row.get("plan_sha256") or
                active_id == row.get("plan_id")):
            die("active plan lineage conflicts with a retirement receipt")


def validate_retirement_state(repo_root, filename=None):
    rows = validate_retirement_ledger(repo_root, filename)
    validate_active_retirement_lineage(repo_root, rows)
    return rows


def run(context):
    global CONTEXT, STATES, ID_RE
    CONTEXT = context
    STATES = context["states"]
    ID_RE = context["id_re"]
    repo_root, parent = retirement_parent()
    path = context["path"]
    phase = env("OMS_RETIRE_PHASE") or "check"
    disposition = env("OMS_DISPOSITION")
    reason = env("OMS_REASON")
    expected_sha = env("OMS_EXPECTED_PLAN_SHA256")
    expected_head = env("OMS_RETIRE_EXPECTED_HEAD")
    expected_ref = env("OMS_RETIRE_EXPECTED_REF")
    expected_generation = env("OMS_RETIRE_EXPECTED_GENERATION")
    expected_proof = env("OMS_RETIRE_PROOF_ID")
    active_payload, active_generation = bounded_regular(
        path, "active plan", 1024 * 1024, absent_ok=True)
    active_sha = hashlib.sha256(active_payload).hexdigest() if active_payload is not None else ""

    if phase == "check":
        if active_payload is None:
            die("no active plan to retire")
        _, tasks = plan_from_payload(active_payload)
        validate_authority(tasks)
        head, git_ref, clean = git_snapshot(repo_root)
        print(json.dumps({
            "schema": 1, "action": "plan-retire-check", "plan_sha256": active_sha,
            "head": head, "git_ref": git_ref, "clean": clean,
            "active_task_blockers": [], "would_write": False,
        }, sort_keys=True))
        return

    if phase not in {"preflight", "finalize"}:
        die("invalid internal retirement phase")
    if disposition not in {"completed-external", "superseded"}:
        die("retirement disposition is invalid")
    if not re.fullmatch(r"[0-9a-f]{64}", expected_sha):
        die("retirement expected plan SHA-256 is invalid")
    archive, intent_path, receipts_path = paths(parent, expected_sha)
    archive_temporary = os.path.join(parent, ".%s.tmp" % os.path.basename(archive))
    recover_link_pair(archive, archive_temporary, parent, "retirement archive")
    prior = matching_receipt(
        validate_retirement_ledger(repo_root, receipts_path), expected_sha)
    if prior is not None:
        prior = validate_receipt(prior, repo_root, archive, expected_sha, disposition, reason)
    intent_temporary = os.path.join(parent, ".%s.tmp" % os.path.basename(intent_path))
    recover_link_pair(intent_path, intent_temporary, parent, "retirement intent")
    intent_payload, _ = bounded_regular(intent_path, "plan retirement intent", 64 * 1024,
                                        absent_ok=True)
    intent = None
    if intent_payload is not None:
        intent = parse_single_jsonl_object(intent_payload, "plan retirement intent")

    if prior is not None and (active_payload is None or
            active_generation != prior.get("plan_generation")):
        if active_payload is not None:
            active_plan, _ = plan_from_payload(active_payload)
            if (active_sha == expected_sha or
                    active_plan.get("plan_id", "") == prior.get("plan_id")):
                die("active plan still belongs to the retired lineage; exact replay is required")
        if intent is not None:
            expected_intent = intent_from_receipt(prior)
            if intent != expected_intent:
                die("completed retirement has a conflicting residual intent")
            os.unlink(intent_path)
            fsync_directory(parent)
        print(json.dumps({
            "schema": 1, "action": "plan-retire", "status": "already-retired",
            "plan_sha256": expected_sha, "archive": prior.get("archive", ""),
            "disposition": disposition, "active_plan_untouched": active_payload is not None,
        }, sort_keys=True))
        return
    if active_payload is None:
        die("retirement is incomplete but its original active plan is missing")
    if active_sha != expected_sha:
        die("active plan changed after the retirement CAS was reviewed")
    plan, tasks = plan_from_payload(active_payload)
    acceptance = plan.get("accept", "")
    if disposition == "completed-external" and (
            not isinstance(acceptance, str) or not acceptance):
        die("completed-external requires a non-empty plan acceptance command")
    validate_authority(tasks)
    if disposition == "superseded" and any(
            task.get("state", "ready") != "done" for task in tasks.values()):
        die("superseded retirement requires every old-plan task to be done")
    head, git_ref, clean = git_snapshot(repo_root)
    if disposition == "completed-external" and not clean:
        die("plan retirement requires a clean committed worktree")

    if prior is not None:
        if intent is None:
            die("receipt-before-unlink recovery requires its exact retirement intent")
        proof_id = prior.get("proof_id", "")
    elif intent is not None:
        if (intent.get("plan_sha256") != expected_sha or
                intent.get("disposition") != disposition or intent.get("reason") != reason or
                intent.get("head") != head or intent.get("git_ref") != git_ref or
                intent.get("plan_generation") != active_generation):
            die("retirement intent does not match the current plan generation")
        proof_id = intent.get("proof_id", "")
    elif phase == "finalize" and re.fullmatch(r"[0-9a-f]{64}", expected_proof):
        proof_id = expected_proof
    else:
        proof_id = secrets.token_hex(32)
    if not isinstance(proof_id, str) or not re.fullmatch(r"[0-9a-f]{64}", proof_id):
        die("retirement proof token is malformed")
    snapshot = {
        "plan_sha256": expected_sha, "head": head, "git_ref": git_ref,
        "plan_generation": active_generation, "proof_id": proof_id,
        "accept_sha256": hashlib.sha256(acceptance.encode("utf-8")).hexdigest()
        if isinstance(acceptance, str) else "",
    }
    if phase == "preflight":
        print(json.dumps(dict(snapshot, schema=1, action="plan-retire-preflight",
                              resume=bool(intent or prior)), sort_keys=True))
        return
    try:
        frozen_generation = json.loads(expected_generation)
    except (TypeError, ValueError):
        die("retirement generation CAS is malformed")
    if (expected_head != head or expected_ref != git_ref or
            frozen_generation != active_generation or expected_proof != proof_id):
        die("plan, HEAD, ref, or file generation changed during retirement")
    proof = acceptance_proof(parent, snapshot) if disposition == "completed-external" else None
    archive_rel = os.path.relpath(archive, repo_root).replace(os.sep, "/")
    intent_row = {
        "schema": 1, "kind": "plan-retirement-intent", "plan_sha256": expected_sha,
        "disposition": disposition, "reason": reason, "head": head,
        "git_ref": git_ref, "plan_generation": active_generation,
        "proof_id": proof_id, "archive": archive_rel,
    }
    if intent is None:
        atomic_intent(intent_path, intent_row, parent)
    elif intent != intent_row:
        die("retirement intent does not match this exact operation")
    durable_archive(archive, active_payload, proof_id, parent)
    receipt = {
        "schema": 1, "kind": "plan-retirement", "ts": context["ts"],
        "retirement_id": "retire_" + proof_id,
        "plan_sha256": expected_sha, "plan_id": plan.get("plan_id", ""),
        "disposition": disposition, "reason": reason, "head": head,
        "git_ref": git_ref, "archive": archive_rel,
        "plan_generation": active_generation, "proof_id": proof_id,
        "acceptance_verified": disposition == "completed-external",
        "task_landings_claimed": False,
        "task_states": dict(sorted(collections.Counter(
            task.get("state", "ready") for task in tasks.values()).items())),
    }
    if proof is not None:
        receipt["acceptance"] = proof
    if prior is None:
        append_receipt(receipts_path, receipt)
    else:
        comparable = dict(receipt, ts=prior.get("ts"),
                          retirement_id=prior.get("retirement_id"))
        if comparable != prior:
            die("existing retirement receipt conflicts with this operation")
    current_payload, current_generation = bounded_regular(
        path, "active plan", 1024 * 1024, absent_ok=False)
    if (current_generation != active_generation or
            hashlib.sha256(current_payload).hexdigest() != expected_sha):
        die("active plan generation changed before retirement unlink")
    os.unlink(path)
    fsync_directory(parent)
    try:
        os.unlink(intent_path)
        fsync_directory(parent)
    except FileNotFoundError:
        pass
    print(json.dumps({
        "schema": 1, "action": "plan-retire", "status": "retired",
        "plan_sha256": expected_sha, "archive": archive_rel,
        "disposition": disposition, "receipt": "retire_" + proof_id,
    }, sort_keys=True))


def accept_context(repo, plan):
    expected_plan = env("OMS_PLAN_ACCEPT_EXPECTED_PLAN_SHA")
    expected_head = env("OMS_PLAN_ACCEPT_EXPECTED_HEAD")
    expected_ref = env("OMS_PLAN_ACCEPT_EXPECTED_REF")
    try:
        expected_generation = json.loads(env("OMS_PLAN_ACCEPT_GENERATION"))
        payload, generation = bounded_regular(plan, "active plan", 1024 * 1024,
                                              absent_ok=False)
        if generation != expected_generation or hashlib.sha256(payload).hexdigest() != expected_plan:
            return False
        head, git_ref, clean = git_snapshot(repo)
        return head == expected_head and git_ref == expected_ref and clean
    except (TypeError, ValueError):
        return False


if __name__ == "__main__":
    if len(sys.argv) != 4 or sys.argv[1] != "accept-context":
        raise SystemExit(2)
    CONTEXT = {"path": sys.argv[3]}
    raise SystemExit(0 if accept_context(os.path.realpath(sys.argv[2]), sys.argv[3]) else 1)
