"""Read-only provenance for a completed Codex Desktop main READ approval.

This is a local provenance check, not authentication against an adversary who
controls this user's account and can rewrite the native session store. The
completion receipt must retain the captured proof hash for later verification.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import time

try:
    import grp
    import pwd
except ImportError:  # Windows can import the module; live capture rejects the host.
    grp = pwd = None


POLICY = "main-read-completion-v1"
ACTION = "accept-main-research"
APPROVE_ACTION = "approve-main-research"
MAX_ROLLOUT = 64 * 1024 * 1024
MAX_STORE_ENTRIES = 20000
MAX_AGE = 120
MAX_CONTINUATION_AGE = 420
HEX64 = re.compile(r"[0-9a-f]{64}\Z")
SID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z")
ID = re.compile(r"[A-Za-z0-9._-]{1,160}\Z")
QUESTION_PREFIX = "직접 작성·검토한 READ 보고서도 완료 증거로 지원하도록 계약을 보완할까요?"
ANSWER = "검증된 메인 READ도 지원"
LEGACY_QUESTION_SHA256 = "5c16741f07d2e3a1ebdff52109fe1a1019674ba3f70229b9e021e5a68b510003"
MACHINE_PREFIX = "[OMS main-read-completion-v1]"
MACHINE_ANSWER = "approve"
CALL_PREFIX = re.compile(r"\A\s*const r\s*=\s*await tools\.exec_command\(\s*", re.S)
CALL_SUFFIX = re.compile(r"\s*\)\s*;\s*text\(r\.output\)\s*;?\s*\Z", re.S)
CAS_FLAGS = ("--repo", "--id", "--lease-id", "--expected-state",
             "--expected-plan-sha256", "--expected-task-sha256",
             "--completion-bundle", "--expected-completion-bundle-sha256")


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate native JSON key")
        result[key] = value
    return result


def _nonfinite_json(value):
    raise ValueError("nonfinite native JSON value: " + value)


def _str(value, label, pattern=None):
    if not isinstance(value, str) or not value or (pattern and not pattern.fullmatch(value)):
        raise ValueError("invalid " + label)
    return value


def _private_group(gid, uid):
    if grp is None or pwd is None:
        raise ValueError("native account ownership is unsupported on this host")
    group = grp.getgrgid(gid)
    return (all(pwd.getpwnam(name).pw_uid == uid for name in group.gr_mem)
            and [item.pw_uid for item in pwd.getpwall() if item.pw_gid == gid] == [uid])


def _secure_path(path, uid, home, leaf=True):
    """Reject links and public writable components from the user's home down."""
    path = os.path.abspath(path)
    home = os.path.abspath(home)
    if path != home and not path.startswith(home + os.sep):
        raise ValueError("native path is outside the account home")
    parts = [home]
    current = home
    for part in os.path.relpath(path, home).split(os.sep):
        if part != ".":
            current = os.path.join(current, part)
            parts.append(current)
    for index, item in enumerate(parts):
        info = os.lstat(item)
        if info.st_uid != uid or stat.S_ISLNK(info.st_mode):
            raise ValueError("native path has an unsafe owner or link")
        if info.st_mode & stat.S_IWOTH or (info.st_mode & stat.S_IWGRP and
                                          not _private_group(info.st_gid, uid)):
            raise ValueError("native path has unsafe write permissions")
        if index == len(parts) - 1 and leaf:
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise ValueError("native record must be one regular file")
        elif not stat.S_ISDIR(info.st_mode):
            raise ValueError("native path component is not a directory")


def _open_parent_chain(path, uid, home):
    """Pin each directory while opening the leaf relative to its parent."""
    path = os.path.abspath(path)
    home = os.path.abspath(home)
    if not path.startswith(home + os.sep):
        raise ValueError("native path is outside the account home")
    components = os.path.relpath(path, home).split(os.sep)
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    flags |= getattr(os, "O_NONBLOCK", 0)
    chain = []
    try:
        fd = os.open(home, flags)
        chain.append((fd, home))
        for component in components[:-1]:
            fd = os.open(component, flags, dir_fd=chain[-1][0])
            chain.append((fd, os.path.join(chain[-1][1], component)))
        _recheck_chain(chain, uid)
        return chain, components[-1]
    except Exception:
        for fd, _ in reversed(chain):
            os.close(fd)
        raise


def _recheck_chain(chain, uid):
    for fd, path in chain:
        held = os.fstat(fd)
        named = os.lstat(path)
        if ((held.st_dev, held.st_ino) != (named.st_dev, named.st_ino)
                or not stat.S_ISDIR(held.st_mode) or held.st_uid != uid
                or stat.S_ISLNK(named.st_mode) or held.st_mode & stat.S_IWOTH
                or (held.st_mode & stat.S_IWGRP and not _private_group(held.st_gid, uid))):
            raise ValueError("native directory identity or permissions changed")


def _read_record(path, uid, home):
    _secure_path(path, uid, home)
    chain, basename = _open_parent_chain(path, uid, home)
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(basename, flags, dir_fd=chain[-1][0])
    except Exception:
        for parent, _ in reversed(chain):
            os.close(parent)
        raise
    try:
        info = os.fstat(fd)
        named = os.stat(basename, dir_fd=chain[-1][0], follow_symlinks=False)
        if ((info.st_dev, info.st_ino) != (named.st_dev, named.st_ino)
                or not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid != uid or info.st_size > MAX_ROLLOUT
                or info.st_mode & stat.S_IWOTH
                or (info.st_mode & stat.S_IWGRP and not _private_group(info.st_gid, uid))):
            raise ValueError("native record changed or exceeded its limit")
        chunks, count = [], 0
        while True:
            part = os.read(fd, min(1024 * 1024, MAX_ROLLOUT + 1 - count))
            if not part:
                break
            count += len(part)
            if count > MAX_ROLLOUT:
                raise ValueError("native record exceeded its limit")
            chunks.append(part)
        after = os.fstat(fd)
        named = os.stat(basename, dir_fd=chain[-1][0], follow_symlinks=False)
        _recheck_chain(chain, uid)
        if ((after.st_dev, after.st_ino) != (named.st_dev, named.st_ino)
                or (after.st_size, after.st_mtime_ns, after.st_ctime_ns) !=
                   (info.st_size, info.st_mtime_ns, info.st_ctime_ns)
                or after.st_nlink != 1 or after.st_uid != uid
                or after.st_mode & stat.S_IWOTH
                or (after.st_mode & stat.S_IWGRP and not _private_group(after.st_gid, uid))
                or count != info.st_size):
            raise ValueError("native record changed during read")
        return b"".join(chunks)
    finally:
        os.close(fd)
        for parent, _ in reversed(chain):
            os.close(parent)


def _find_rollout(sessions, selector, uid, home, now):
    del now  # all retained and archived sessions participate in uniqueness.
    sid_selector = isinstance(selector, str) and SID.fullmatch(selector)
    consumer_selector = isinstance(selector, str) and re.fullmatch(r"[0-9a-f]{32}", selector)
    if not sid_selector and not consumer_selector:
        raise ValueError("invalid native session selector")
    roots = [sessions, os.path.join(os.path.dirname(sessions), "archived_sessions")]
    found, visited, seen = [], [], 0
    for root in roots:
        if not os.path.lexists(root):
            if root == sessions:
                raise ValueError("native session store is absent")
            continue
        pending = [(root, 0)]
        while pending:
            folder, depth = pending.pop()
            if depth > 5:
                raise ValueError("native session store exceeds traversal depth")
            _secure_path(folder, uid, home, leaf=False)
            flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
            fd = os.open(folder, flags)
            try:
                info = os.fstat(fd)
                with os.scandir(fd) as entries:
                    rows = list(entries)
                named = os.lstat(folder)
                if (info.st_dev, info.st_ino) != (named.st_dev, named.st_ino):
                    raise ValueError("native session directory changed during scan")
                visited.append((folder, info.st_dev, info.st_ino))
            finally:
                os.close(fd)
            seen += len(rows)
            if seen > MAX_STORE_ENTRIES:
                raise ValueError("native session store exceeds entry limit")
            for item in rows:
                if item.is_symlink():
                    raise ValueError("native session store contains a link")
                if item.is_dir(follow_symlinks=False):
                    pending.append((os.path.join(folder, item.name), depth + 1))
                elif item.name.startswith("rollout-") and item.name.endswith(".jsonl"):
                    candidate = item.name[-42:-6]
                    if (SID.fullmatch(candidate) and
                            ((sid_selector and candidate == selector) or
                             (consumer_selector and _sha(candidate.encode())[:32] == selector))):
                        found.append(os.path.join(folder, item.name))
    for folder, dev, ino in visited:
        info = os.lstat(folder)
        if (info.st_dev, info.st_ino) != (dev, ino):
            raise ValueError("native session store changed during traversal")
    if len(found) != 1:
        raise ValueError("native session is absent or ambiguous")
    return found[0]


def _parse_exec_input(source):
    if not isinstance(source, str) or len(source) > 16384:
        raise ValueError("unsupported native exec input")
    match = CALL_PREFIX.match(source)
    if not match:
        raise ValueError("native exec input is not a direct command")
    decoder = json.JSONDecoder(object_pairs_hook=_unique_object,
                               parse_constant=_nonfinite_json)
    obj, end = decoder.raw_decode(source, match.end())
    if not CALL_SUFFIX.fullmatch(source[end:]) or not isinstance(obj, dict) or set(obj) != {"cmd", "workdir"}:
        raise ValueError("native exec input has extra operations")
    if not all(isinstance(obj[key], str) for key in obj):
        raise ValueError("native exec arguments are invalid")
    return obj


def _argv(cmd, execution_repo, task_id, bundle_sha, expected, state_repo=None,
          action=ACTION, approval=False):
    if not isinstance(expected, list) or any(not isinstance(item, str) for item in expected):
        raise ValueError("expected invocation must be an argv list")
    if any(char in cmd for char in "\n\r\x00;&|<>`$(){}"):
        raise ValueError("shell command contains an operator")
    argv = shlex.split(cmd, posix=True)
    if argv != expected:
        raise ValueError("native command differs from the requested invocation")
    if approval:
        if not argv or argv[-1] != "--approve-reviewed-bundle":
            raise ValueError("reviewer approval flag is absent")
        argv = argv[:-1]
    state_repo = execution_repo if state_repo is None else state_repo
    if state_repo != execution_repo:
        if argv[:2] != ["env", "OMS_STATE_REPO=" + state_repo]:
            raise ValueError("native command lacks the exact state repository binding")
        argv = argv[2:]
    if len(argv) != 3 + 2 * len(CAS_FLAGS) or argv[:3] != ["scripts/oms", "agent-plan", action]:
        raise ValueError("native command does not call the acceptance action")
    pairs = argv[3:]
    if tuple(pairs[::2]) != CAS_FLAGS or any(not item for item in pairs[1::2]):
        raise ValueError("native command has missing or reordered CAS flags")
    values = dict(zip(pairs[::2], pairs[1::2]))
    if (values["--repo"] != execution_repo or values["--id"] != task_id
            or values["--expected-completion-bundle-sha256"] != bundle_sha):
        raise ValueError("acceptance target differs from the requested target")
    if not all(HEX64.fullmatch(values[key]) for key in
               ("--expected-plan-sha256", "--expected-task-sha256",
                "--expected-completion-bundle-sha256")):
        raise ValueError("invalid CAS digest")
    bundle = values["--completion-bundle"]
    if (os.path.isabs(bundle) or "\\" in bundle or any(part in ("", ".", "..") for part in bundle.split("/"))):
        raise ValueError("completion bundle must be repository-relative")
    return values


def _approve_argv(cmd, execution_repo, task_id, bundle_sha, expected, state_repo):
    return _argv(cmd, execution_repo, task_id, bundle_sha, expected, state_repo,
                 APPROVE_ACTION, True)


def _approval(row, raw, descriptor):
    value = row.get("payload", {})
    if (row.get("type") != "response_item" or value.get("type") != "message"
            or value.get("role") != "user" or value.get("id") != descriptor.get("message_id")):
        return None
    content = value.get("content")
    if (not isinstance(content, list) or len(content) != 1 or
            not isinstance(content[0], dict) or content[0].get("type") != "input_text"):
        raise ValueError("approval is not a structured user message")
    text = content[0].get("text", "")
    start, end = "<send_user_message_question_reply>\n", "\n</send_user_message_question_reply>\n"
    if not isinstance(text, str) or not text.startswith(start) or not text.endswith(end):
        raise ValueError("approval is not a structured question reply")
    replies = json.loads(text[len(start):-len(end)], object_pairs_hook=_unique_object,
                         parse_constant=_nonfinite_json)
    if not isinstance(replies, list) or not replies:
        raise ValueError("approval reply is empty")
    selected = [item for item in replies if isinstance(item, dict) and
                item.get("questionItemId") == descriptor.get("question_item_id")]
    if len(selected) != 1:
        raise ValueError("approval question id is absent or ambiguous")
    item = selected[0]
    question = item.get("question")
    answer = item.get("answer")
    if (isinstance(question, str) and question.startswith(QUESTION_PREFIX) and
            _sha(question.encode()) == LEGACY_QUESTION_SHA256 and answer == ANSWER):
        adapter = "legacy-korean-v1"
    elif (isinstance(question, str) and question.startswith(MACHINE_PREFIX + " ")
          and question[len(MACHINE_PREFIX):].strip() and answer == MACHINE_ANSWER):
        adapter = "canonical-v1"
    else:
        raise ValueError("approval did not affirm the main READ completion policy")
    if (descriptor.get("policy") != POLICY
            or _sha(question.encode()) != descriptor.get("question_sha256")
            or _sha(answer.encode()) != descriptor.get("answer_sha256")):
        raise ValueError("approval policy or answer differs")
    return ({"message_id": value["id"], "question_item_id": item["questionItemId"],
             "question_sha256": _sha(question.encode()), "answer_sha256": _sha(answer.encode()),
             "record_sha256": _sha(raw), "policy_adapter": adapter}, question, answer)


def _prompt_reference(descriptor):
    raw = descriptor.get("question_item_id")
    if not isinstance(raw, str) or len(raw) > 512:
        raise ValueError("invalid native question reference")
    ref = json.loads(raw)
    if (not isinstance(ref, list) or len(ref) != 3 or ref[0] != "request_user_input_async"
            or not isinstance(ref[1], str) or not re.fullmatch(r"call_[A-Za-z0-9_-]{1,128}", ref[1])
            or type(ref[2]) is not int or not 0 <= ref[2] < 3):
        raise ValueError("invalid native question reference")
    return ref[1], ref[2]


def _prompt(row, raw, call_id, question_index):
    value = row.get("payload", {})
    if (row.get("type") != "response_item" or value.get("type") != "function_call"
            or value.get("name") != "request_user_input_async" or value.get("call_id") != call_id):
        return None
    arguments = value.get("arguments")
    if not isinstance(arguments, str) or len(arguments) > 16384:
        raise ValueError("native approval prompt arguments are invalid")
    payload = json.loads(arguments, object_pairs_hook=_unique_object,
                         parse_constant=_nonfinite_json)
    if not isinstance(payload, dict) or set(payload) != {"questions"}:
        raise ValueError("native approval prompt fields are invalid")
    questions = payload["questions"]
    if not isinstance(questions, list) or not 0 <= question_index < len(questions) <= 3:
        raise ValueError("native approval prompt index is invalid")
    item = questions[question_index]
    if not isinstance(item, dict) or set(item) != {"title", "options"}:
        raise ValueError("native approval question fields are invalid")
    title, options = item["title"], item["options"]
    if (not isinstance(title, str) or not title or not isinstance(options, list)
            or not 2 <= len(options) <= 3 or any(not isinstance(option, str) or not option for option in options)
            or len(set(options)) != len(options)):
        raise ValueError("native approval options are invalid")
    return {"title": title, "options": options,
            "record_sha256": _sha(raw), "call_id_sha256": _sha(call_id.encode())}


def _completed_approval_evidence(data, sid, state_repo, request, invocation, envelope):
    """Bind one direct native tool call to its completed backend execution."""
    workdir = request.get("execution_repo")
    if not isinstance(workdir, str) or not os.path.isabs(workdir):
        raise ValueError("native approval execution directory is invalid")
    backend_workdirs = {workdir, Path(workdir).as_uri()}
    if not data.endswith(b"\n"):
        raise ValueError("native approval rollout has an incomplete row")
    raw_rows = data.splitlines(keepends=True)
    if not raw_rows or len(raw_rows) > 100000:
        raise ValueError("native approval rollout has an invalid row count")
    rows = [json.loads(raw, object_pairs_hook=_unique_object,
                       parse_constant=_nonfinite_json) for raw in raw_rows]
    if any(not isinstance(row, dict) or not isinstance(row.get("payload"), dict) for row in rows):
        raise ValueError("native approval rollout has an invalid row")
    header = rows[0]
    meta = header["payload"]
    if (header.get("type") != "session_meta" or meta.get("id") != sid or
            meta.get("session_id") not in (None, sid) or meta.get("cwd") != state_repo or
            meta.get("originator") != "Codex Desktop" or
            meta.get("source") not in ("vscode", "desktop") or
            meta.get("thread_source") != "user"):
        raise ValueError("approval producer is not a root Desktop session")
    descriptor = request.get("approval")
    if not isinstance(descriptor, dict):
        raise ValueError("native user policy approval descriptor is missing")
    prompt_id, question_index = _prompt_reference(descriptor)
    prompt, prompt_index = None, None
    user, user_index = None, None
    candidates = []
    for index, (row, raw) in enumerate(zip(rows[1:], raw_rows[1:]), 1):
        proposed = _prompt(row, raw, prompt_id, question_index)
        if proposed is not None:
            if prompt is not None:
                raise ValueError("native policy prompt is duplicated")
            prompt, prompt_index = proposed, index
        proposed = _approval(row, raw, descriptor)
        if proposed is not None:
            if user is not None:
                raise ValueError("native policy reply is duplicated")
            user, user_index = proposed, index
        value = row["payload"]
        if (row.get("type") == "response_item" and value.get("type") == "custom_tool_call"
                and value.get("name") == "exec" and value.get("status") in ("completed", "in_progress")):
            source = value.get("input")
            try:
                call = _parse_exec_input(source)
            except (TypeError, ValueError, json.JSONDecodeError):
                continue
            if call == {"cmd": invocation, "workdir": request.get("execution_repo")}:
                candidates.append((index, row, raw))
    if (prompt is None or user is None or prompt_index >= user_index or
            prompt["title"] != user[1] or user[2] not in prompt["options"]):
        raise ValueError("actual native user policy approval is absent")
    approval = dict(user[0], prompt_record_sha256=prompt["record_sha256"],
                    prompt_call_id_sha256=prompt["call_id_sha256"])
    matched = []
    for index, row, raw in candidates:
        if index <= user_index:
            continue
        call_id = _str(row["payload"].get("call_id"), "native approval call id")
        if (sum(item.get("type") == "response_item" and
                item["payload"].get("type") == "custom_tool_call" and
                item["payload"].get("call_id") == call_id for item in rows) != 1 or
                sum(item.get("type") == "response_item" and
                    item["payload"].get("type") == "custom_tool_call_output" and
                    item["payload"].get("call_id") == call_id for item in rows) != 1):
            raise ValueError("native approval tool call id is ambiguous")
        outputs = [(pos, item, raw_rows[pos]) for pos, item in enumerate(rows[index + 1:], index + 1)
                   if (item.get("type") == "response_item" and
                       item["payload"].get("type") == "custom_tool_call_output" and
                       item["payload"].get("call_id") == call_id)]
        if len(outputs) != 1:
            raise ValueError("native approval call output is missing or duplicated")
        output_index, output_row, output_raw = outputs[0]
        if any(other.get("type") == "response_item" and
               other["payload"].get("type") == "custom_tool_call" and
               other["payload"].get("name") == "exec" for other in rows[index + 1:output_index]):
            raise ValueError("native approval call is interleaved with another exec")
        blocks = output_row["payload"].get("output")
        if (not isinstance(blocks, list) or len(blocks) != 2 or
                any(not isinstance(block, dict) or block.get("type") != "input_text" or
                    set(block) != {"type", "text"} for block in blocks) or
                not isinstance(blocks[0]["text"], str) or
                re.fullmatch(r"Script completed\nWall time [0-9]+(?:\.[0-9]+)? seconds\nOutput:\n",
                             blocks[0]["text"]) is None or blocks[1]["text"] != envelope):
            continue
        events = [(pos, item, raw_rows[pos]) for pos, item in enumerate(rows[index + 1:output_index], index + 1)
                  if item.get("type") == "event_msg" and
                     item["payload"].get("type") == "item_completed" and
                     isinstance(item["payload"].get("item"), dict) and
                     item["payload"]["item"].get("type") == "CommandExecution"]
        if len(events) != 1:
            continue
        _, event, event_raw = events[0]
        event_payload = event["payload"]
        item = event_payload["item"]
        if (event_payload.get("thread_id") != sid or item.get("source") != "unified_exec_startup"
                or item.get("command") != ["/bin/bash", "-lc", invocation]
                or item.get("cwd") not in backend_workdirs
                or item.get("status") != "completed" or type(item.get("exit_code")) is not int
                or item["exit_code"] != 0 or item.get("aggregated_output") != envelope
                or not isinstance(item.get("id"), str) or not item["id"].startswith("exec-")
                or not isinstance(item.get("process_id"), str) or not item["process_id"]):
            continue
        matched.append((row, raw, event, event_raw, output_row, output_raw))
    if len(matched) != 1:
        raise ValueError("completed native main approval is absent or ambiguous")
    call, call_raw, event, event_raw, output, output_raw = matched[0]
    value = call["payload"]
    item = event["payload"]["item"]
    # The raw backend cwd spelling is bound by completion_record_sha256. Store
    # the directory it proved as a stable digest for historical receipt checks.
    native = {"call_record_sha256": _sha(call_raw), "call_input_sha256": _sha(value["input"].encode()),
              "call_id_sha256": _sha(value["call_id"].encode()),
              "completion_record_sha256": _sha(event_raw),
              "backend_item_id_sha256": _sha(item["id"].encode()),
              "backend_process_id_sha256": _sha(item["process_id"].encode()),
              "output_record_sha256": _sha(output_raw), "envelope_sha256": _sha(envelope.encode()),
              "workdir_sha256": _sha(workdir.encode())}
    return approval, native, _sha(raw_rows[0])


def _room_approver(repo, room_id, participant):
    library = os.path.dirname(__file__)
    if library not in sys.path:
        sys.path.insert(0, library)
    import room
    state = room.project(room.records(repo, room_id))
    member = room.participant(state, participant)
    consumer = member.get("consumer")
    if (state["id"] != room_id or state["closed"] or not member.get("joined")
            or member.get("role") != "main" or member.get("provider") != "codex"
            or member.get("parent") or not isinstance(consumer, str)
            or re.fullmatch(r"[0-9a-f]{32}", consumer) is None):
        raise ValueError("approval producer is not a bound Desktop room main")
    binding = _sha(_canonical({"room": room_id, "participant": participant,
                               "consumer": consumer, "role": "main", "provider": "codex"}))
    return consumer, binding


def _git_identity(repo):
    root = subprocess.run(["git", "-C", repo, "rev-parse", "--show-toplevel"],
                          capture_output=True, text=True, timeout=5, check=True).stdout.strip()
    common = subprocess.run(["git", "-C", repo, "rev-parse", "--git-common-dir"],
                            capture_output=True, text=True, timeout=5, check=True).stdout.strip()
    if os.path.realpath(root) != repo or not common:
        raise ValueError("Git top level differs from requested repository")
    return os.path.realpath(common if os.path.isabs(common) else os.path.join(repo, common))


def _approval_marker(approval_ref, request, repo, execution_repo, task_id, bundle_sha,
                     room_id, participant, common_sha, values):
    if (not isinstance(approval_ref, dict) or set(approval_ref) != {"path", "sha256"}
            or not isinstance(approval_ref.get("sha256"), str)
            or not HEX64.fullmatch(approval_ref["sha256"]) or
            approval_ref.get("path") != ".oms/plan/completion-captures/" + approval_ref["sha256"] + ".json"):
        raise ValueError("main approval reference is not canonical")
    marker = request.get("approved_marker")
    if not isinstance(marker, dict):
        raise ValueError("strictly read main approval marker is missing")
    raw = _canonical(marker) + b"\n"
    if _sha(raw) != approval_ref["sha256"]:
        raise ValueError("main approval marker differs from immutable reference")
    expected = {"schema": 1, "kind": "oms-main-read-approval-v1",
                "authorized_action": ACTION, "task_id": task_id,
                "bundle_sha256": bundle_sha, "room_id": room_id, "participant": participant,
                "lease_id": values["--lease-id"], "state": values["--expected-state"],
                "plan_sha256": values["--expected-plan-sha256"],
                "task_sha256": values["--expected-task-sha256"],
                "snapshot_sha256": request.get("expected_snapshot_sha256"),
                "code_manifest_sha256": request.get("code_manifest_sha256"),
                "state_repo_sha256": _sha(repo.encode()),
                "execution_repo_sha256": _sha(execution_repo.encode()),
                "git_common_sha256": common_sha}
    for key, value in expected.items():
        if marker.get(key) != value:
            raise ValueError("main approval marker changed its reviewed target")
    if (type(marker.get("schema")) is not int or
            not isinstance(marker.get("nonce"), str) or not HEX64.fullmatch(marker["nonce"]) or
            not isinstance(expected["snapshot_sha256"], str) or
            not HEX64.fullmatch(expected["snapshot_sha256"]) or
            not isinstance(expected["code_manifest_sha256"], str) or
            not HEX64.fullmatch(expected["code_manifest_sha256"])):
        raise ValueError("main approval marker has invalid frozen digests")
    envelope = _canonical({"kind": "oms-main-read-approved", "approval": approval_ref}).decode() + "\n"
    return marker, envelope


def capture_completed_main_reviewer(state_repo, task_id, bundle_sha, room_id, participant,
                                   approval_ref, request):
    """Capture a completed main approval from its native producer session.

    The caller may be another local executor. Its SID or shared app-server is
    never attributed to the approval producer.
    """
    if os.name != "posix" or not os.path.isdir("/proc/self") or not hasattr(os, "getuid"):
        raise ValueError("Desktop native approval provenance is unsupported on this host")
    uid = os.getuid()
    home = pwd.getpwuid(uid).pw_dir
    repo = os.path.realpath(_str(state_repo, "state repository"))
    _str(task_id, "task id", ID)
    _str(bundle_sha, "bundle digest", HEX64)
    _str(room_id, "room id", ID)
    _str(participant, "participant", ID)
    if not isinstance(request, dict) or request.get("policy") != POLICY:
        raise ValueError("unsupported completed main reviewer request")
    execution_repo = os.path.realpath(_str(request.get("execution_repo"), "execution repository"))
    if state_repo != repo or request["execution_repo"] != execution_repo:
        raise ValueError("repository spelling is not canonical")
    if (os.environ.get("OMS_HARNESS_CHILD") not in (None, "", "0") or
            os.environ.get("OMS_HARNESS_DELEGATE_DEPTH") not in (None, "", "0") or
            any(os.environ.get(key) for key in ("OMS_ATTEMPT_ID", "OMS_HARNESS_CALL_ID",
                                                "OMS_PANEL_MAIN_ATTEMPT", "OMS_PANEL_SESSION",
                                                "OMS_PANEL_REPO", "OMS_PANEL_HOST",
                                                "OMS_PANEL_POSITION", "OMS_PANEL_ENTRYPOINT",
                                                "OMS_PANEL_RESULTS", "GIT_DIR", "GIT_WORK_TREE",
                                                "GIT_COMMON_DIR", "GIT_NAMESPACE", "GIT_INDEX_FILE"))):
        raise ValueError("child, panel, or alternate Git namespace is unsupported")
    if os.environ.get("OMS_STATE_REPO") not in (None, "", repo) or (
            repo != execution_repo and os.environ.get("OMS_STATE_REPO") != repo):
        raise ValueError("state repository differs from local execution")
    if os.path.realpath(os.getcwd()) != execution_repo:
        raise ValueError("execution directory differs from requested source repository")
    common = _git_identity(repo)
    if _git_identity(execution_repo) != common:
        raise ValueError("execution and state repositories have different Git namespaces")
    argv = request.get("approve_argv")
    if not isinstance(argv, list) or any(not isinstance(item, str) for item in argv):
        raise ValueError("expected completed approval invocation is missing")
    invocation = shlex.join(argv)
    values = _approve_argv(invocation, execution_repo, task_id, bundle_sha, argv, repo)
    common_sha = _sha(common.encode())
    marker, envelope = _approval_marker(approval_ref, request, repo, execution_repo, task_id,
                                        bundle_sha, room_id, participant, common_sha, values)
    consumer, room_sha = _room_approver(repo, room_id, participant)
    sessions = os.path.join(home, ".codex", "sessions")
    path = _find_rollout(sessions, consumer, uid, home, time.time())
    sid = os.path.basename(path)[-42:-6]
    if not SID.fullmatch(sid) or _sha(sid.encode())[:32] != consumer:
        raise ValueError("native approval session does not match room consumer")
    approval, native, header_sha = _completed_approval_evidence(
        _read_record(path, uid, home), sid, repo, request, invocation, envelope)
    proof = {"schema": 2, "policy": POLICY, "action": APPROVE_ACTION,
             "authorized_action": ACTION,
             "task_id": task_id, "bundle_sha256": bundle_sha,
             "room_id": room_id, "participant": participant,
             "approval_ref": approval_ref,
             "marker_nonce_sha256": _sha(marker["nonce"].encode()),
             "snapshot_sha256": marker["snapshot_sha256"],
             "code_manifest_sha256": marker["code_manifest_sha256"],
             "state_repo_sha256": _sha(repo.encode()),
             "execution_repo_sha256": _sha(execution_repo.encode()),
             "state_git_namespace_sha256": common_sha,
             "source_git_namespace_sha256": common_sha,
             "git_common_sha256": common_sha,
             "native_consumer": consumer, "session_id_sha256": _sha(sid.encode()),
             "header_sha256": header_sha, "room_binding_sha256": room_sha,
             "approve_invocation_sha256": _sha(invocation.encode()),
             "native_approval_execution": native, "policy_approval": approval}
    proof["proof_sha256"] = _sha(_canonical(proof))
    return proof



def verify_captured_main_reviewer(proof, expected):
    """Validate a receipt-bound completed approval without replaying a chat."""
    if not isinstance(proof, dict) or not isinstance(expected, dict):
        return False
    required = {"schema", "policy", "action", "authorized_action", "task_id", "bundle_sha256", "room_id",
                "participant", "approval_ref", "marker_nonce_sha256", "snapshot_sha256",
                "code_manifest_sha256", "state_repo_sha256", "execution_repo_sha256",
                "state_git_namespace_sha256", "source_git_namespace_sha256",
                "git_common_sha256", "native_consumer", "session_id_sha256",
                "header_sha256", "room_binding_sha256", "approve_invocation_sha256",
                "native_approval_execution", "policy_approval", "proof_sha256"}
    if set(proof) != required:
        return False
    body = dict(proof)
    digest = body.pop("proof_sha256")
    try:
        if not isinstance(digest, str) or not HEX64.fullmatch(digest) or digest != _sha(_canonical(body)):
            return False
    except (TypeError, ValueError):
        return False
    bound = ("schema", "policy", "action", "authorized_action", "task_id", "bundle_sha256", "room_id",
             "participant", "approval_ref", "marker_nonce_sha256", "snapshot_sha256",
             "code_manifest_sha256", "state_repo_sha256", "execution_repo_sha256",
             "state_git_namespace_sha256", "source_git_namespace_sha256",
             "git_common_sha256", "native_consumer", "room_binding_sha256",
             "approve_invocation_sha256", "proof_sha256")
    if any(proof.get(key) != expected.get(key) for key in bound):
        return False
    if (type(proof["schema"]) is not int or proof["schema"] != 2 or
            proof["policy"] != POLICY or proof["action"] != APPROVE_ACTION or
            proof["authorized_action"] != ACTION or
            any(not isinstance(proof.get(key), str) or not ID.fullmatch(proof[key])
                for key in ("task_id", "room_id", "participant"))):
        return False
    digest_keys = ("bundle_sha256", "marker_nonce_sha256", "snapshot_sha256",
                   "code_manifest_sha256", "state_repo_sha256", "execution_repo_sha256",
                   "state_git_namespace_sha256", "source_git_namespace_sha256",
                   "git_common_sha256", "session_id_sha256", "header_sha256",
                   "room_binding_sha256", "approve_invocation_sha256")
    if any(not isinstance(proof.get(key), str) or not HEX64.fullmatch(proof[key])
           for key in digest_keys):
        return False
    if (proof["state_git_namespace_sha256"] != proof["source_git_namespace_sha256"] or
            proof["source_git_namespace_sha256"] != proof["git_common_sha256"] or
            not isinstance(proof["native_consumer"], str) or
            re.fullmatch(r"[0-9a-f]{32}", proof["native_consumer"]) is None or
            proof["native_consumer"] != proof["session_id_sha256"][:32]):
        return False
    ref = proof["approval_ref"]
    if (not isinstance(ref, dict) or set(ref) != {"path", "sha256"} or
            not isinstance(ref.get("sha256"), str) or not HEX64.fullmatch(ref["sha256"]) or
            ref.get("path") != ".oms/plan/completion-captures/" + ref["sha256"] + ".json"):
        return False
    native = proof["native_approval_execution"]
    native_keys = {"call_record_sha256", "call_input_sha256", "call_id_sha256",
                   "completion_record_sha256", "backend_item_id_sha256",
                   "backend_process_id_sha256", "output_record_sha256", "envelope_sha256",
                   "workdir_sha256"}
    if (not isinstance(native, dict) or set(native) != native_keys or
            any(not isinstance(value, str) or not HEX64.fullmatch(value)
                for value in native.values())):
        return False
    envelope = _canonical({"kind": "oms-main-read-approved", "approval": ref}) + b"\n"
    if (native["envelope_sha256"] != _sha(envelope) or
            native["workdir_sha256"] != proof["execution_repo_sha256"]):
        return False
    approval = proof["policy_approval"]
    approval_keys = {"message_id", "question_item_id", "question_sha256",
                     "answer_sha256", "record_sha256", "prompt_record_sha256",
                     "prompt_call_id_sha256", "policy_adapter"}
    declared = expected.get("policy_approval")
    if (not isinstance(approval, dict) or set(approval) != approval_keys or
            not isinstance(declared, dict) or declared.get("policy") != POLICY or
            any(approval.get(key) != declared.get(key)
                for key in ("message_id", "question_item_id", "question_sha256", "answer_sha256")) or
            not isinstance(approval.get("message_id"), str) or not approval["message_id"] or
            not isinstance(approval.get("question_item_id"), str)):
        return False
    if any(not isinstance(approval.get(key), str) or not HEX64.fullmatch(approval[key])
           for key in ("question_sha256", "answer_sha256", "record_sha256",
                       "prompt_record_sha256", "prompt_call_id_sha256")):
        return False
    try:
        prompt_id, _ = _prompt_reference(approval)
    except (TypeError, ValueError, json.JSONDecodeError):
        return False
    if approval["prompt_call_id_sha256"] != _sha(prompt_id.encode()):
        return False
    return ((approval["policy_adapter"] == "legacy-korean-v1" and
             approval["question_sha256"] == LEGACY_QUESTION_SHA256 and
             approval["answer_sha256"] == _sha(ANSWER.encode())) or
            (approval["policy_adapter"] == "canonical-v1" and
             approval["answer_sha256"] == _sha(MACHINE_ANSWER.encode())))
