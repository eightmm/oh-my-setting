"""agent-plan.sh's plan engine, loaded as a module so Python caches its
bytecode: inline, its 1,300 lines were compiled on every agent-plan call, about
ten per goal-drive cycle. sys.argv keeps the heredoc's positional contract."""
import importlib.util as _importlib_util


def _load(path):
    """Like runpy.run_path(path), but through the bytecode cache."""
    name = "_oms_" + "".join(ch if ch.isalnum() else "_" for ch in path.rsplit("/", 1)[-1][:-3])
    spec = _importlib_util.spec_from_file_location(name, path)
    module = _importlib_util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return vars(module)


import datetime, hashlib, json, math, os, re, secrets, stat, subprocess, sys, tempfile, unicodedata

SCHEMA = 3
path = os.environ["OMS_PLAN_FILE"]
act = os.environ["OMS_ACTION"]
ts = os.environ["OMS_TS"]
proposal_path = sys.argv[1]
landing_receipt_digest = _load(sys.argv[2])["digest"]
process_liveness = _load(sys.argv[4])
process_pid_alive = process_liveness["pid_alive"]
persisted_native_pid_is_proven = process_liveness[
    "persisted_native_pid_is_proven"
]
path_scope = _load(sys.argv[6])
within_envelope = path_scope["within_envelope"]
project_state_snapshot = _load(sys.argv[7])["snapshot"]
validate_assignment = _load(sys.argv[8])["validate"]
def env(k): return os.environ.get(k, "")

STATES = {"ready", "claimed", "running", "review", "landing", "blocked", "done"}
ID_RE = re.compile(r"^[A-Za-z0-9._-]+$")
PLAN_ID_RE = re.compile(r"^plan_[0-9a-f]{32}$")
OWNER_RE = re.compile(r"^owner_[0-9a-f]{32}$")

def die(msg):
    sys.stderr.write("error: %s\n" % msg); sys.exit(2)

def load():
    if not os.path.exists(path):
        return {"schema": SCHEMA, "goal": "", "accept": "", "tasks": {}}
    with open(path, encoding="utf-8") as fh:
        d = json.load(fh)
    d.setdefault("tasks", {})
    d.setdefault("accept", "")   # schema 2 plans predate the acceptance contract
    d["schema"] = SCHEMA
    if "plan_id" in d and not PLAN_ID_RE.fullmatch(str(d.get("plan_id", ""))):
        die("plan_id is malformed; refusing to replace immutable lineage")
    for task in d["tasks"].values():
        try:
            validate_assignment(task.get("assignment", {}))
        except ValueError as exc:
            die(str(exc))
        task.setdefault("lease_epoch", 0)
        task.setdefault("lease_id", "")
        task.setdefault("repair_count", 0)
        task.setdefault("repair_artifact", "")
        task.setdefault("executor_id", "")
        task.setdefault("executor_soul_sha256", "")
        task.setdefault("autopilot_owner_id", "")
    return d

def ensure_plan_id(d):
    value = d.get("plan_id")
    if value is None:
        value = "plan_" + secrets.token_hex(16)
        d["plan_id"] = value
    if not isinstance(value, str) or not PLAN_ID_RE.fullmatch(value):
        die("plan_id is malformed; refusing to replace immutable lineage")
    return value

def save(d):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(d, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, path)   # atomic
    except Exception:
        os.unlink(tmp); raise

def split_list(s):
    return [x.strip() for x in re.split(r"[,\s]+", s) if x.strip()]

def read_regular_bytes(filename, label, maximum):
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(filename, flags)
    except OSError as exc:
        die("cannot open %s: %s" % (label, exc))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            die("%s must be a regular file" % label)
        if info.st_size > maximum:
            die("%s exceeds %d bytes" % (label, maximum))
        chunks = []
        total = 0
        while True:
            chunk = os.read(fd, min(1024 * 1024, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > maximum:
                die("%s exceeds %d bytes" % (label, maximum))
        return b"".join(chunks)
    finally:
        os.close(fd)

def commit_landing_proof(sha):
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", sha):
        die("--landed-commit must be a full lowercase commit SHA")
    directory = env("OMS_LAND_RECEIPTS_DIR")
    try:
        candidates = sorted(os.listdir(directory))
    except FileNotFoundError:
        candidates = []
    except OSError as exc:
        die("cannot read oms land receipts: %s" % exc)
    passed = False
    for name in candidates:
        if not name.startswith(sha + "-") or not name.endswith(".json"):
            continue
        raw = read_regular_bytes(os.path.join(directory, name), "oms land receipt", 1024 * 1024)
        try:
            receipt = json.loads(raw)
        except (ValueError, UnicodeError):
            continue
        if not isinstance(receipt, dict) or receipt.get("schema") != 1:
            continue
        if receipt.get("sha") != sha or receipt.get("state") != "passed":
            continue
        gate, push, ci = (receipt.get(key) for key in ("gate", "push", "ci"))
        if not all(isinstance(stage, dict) for stage in (gate, push, ci)):
            continue
        if (type(gate.get("rc")) is not int or gate["rc"] != 0 or
                type(push.get("rc")) is not int or push["rc"] != 0 or
                ci.get("conclusion") != "success"):
            continue
        passed = True
        remote, target = receipt.get("remote"), receipt.get("target")
        if not isinstance(remote, str) or not remote or not isinstance(target, str) or not target:
            continue
        ref = "refs/remotes/%s/%s" % (remote, target)
        # An ancestry read in a partial clone must not lazily fetch objects.
        git_env = dict(os.environ, GIT_NO_LAZY_FETCH="1", GIT_ALLOW_PROTOCOL="")
        try:
            valid = subprocess.run(["git", "check-ref-format", ref],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            reachable = valid.returncode == 0 and subprocess.run(
                ["git", "-C", env("OMS_REPO"), "merge-base", "--is-ancestor", sha, ref],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                env=git_env, timeout=10).returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            reachable = False
        if reachable:
            return {"kind": "commit", "sha": sha,
                    "receipt_sha256": hashlib.sha256(raw).hexdigest()}
    if passed:
        die("landed commit %s is not reachable from a passed receipt's local pushed target ref" % sha)
    die("no passed oms land receipt for %s with gate/push ok and CI success" % sha)

def landed_range_paths(sha):
    """Files the landing of sha changed. The range starts at the nearest
    first-parent ancestor with its own pushed land receipt (the tip that land
    pushed onto); with none, it is sha's own commit against its first parent."""
    directory = env("OMS_LAND_RECEIPTS_DIR")
    try:
        names = os.listdir(directory)
    except OSError:
        names = []
    pushed = set()
    for name in names:
        prior = name.split("-", 1)[0]
        if (prior == sha or not name.endswith(".json") or
                not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", prior)):
            continue
        raw = read_regular_bytes(os.path.join(directory, name), "oms land receipt", 1024 * 1024)
        try:
            receipt = json.loads(raw)
        except (ValueError, UnicodeError):
            continue
        push = receipt.get("push") if isinstance(receipt, dict) else None
        if (isinstance(push, dict) and receipt.get("sha") == prior and
                type(push.get("rc")) is int and push["rc"] == 0):
            pushed.add(prior)
    git_env = dict(os.environ, GIT_NO_LAZY_FETCH="1", GIT_ALLOW_PROTOCOL="")
    def git(*args):
        try:
            run = subprocess.run(["git", "-C", env("OMS_REPO")] + list(args),
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                 env=git_env, timeout=30)
        except (OSError, subprocess.TimeoutExpired) as exc:
            run = exc
        if not isinstance(run, subprocess.CompletedProcess) or run.returncode != 0:
            die("cannot list files changed by landed commit %s; retry where its history is local" % sha)
        return run.stdout.decode("utf-8", "surrogateescape")
    chain = git("rev-list", "--first-parent", "--max-count=1000", sha).split()
    base = next((c for c in chain[1:] if c in pushed), chain[1] if len(chain) > 1 else "")
    diff = ["diff-tree", "-r", "-z", "--name-only", "--no-commit-id"]
    paths = git(*(diff + ([base, sha] if base else ["--root", sha]))).split("\0")
    return ("%s..%s" % (base, sha) if base else "root commit %s" % sha), [x for x in paths if x]

def reject_controls(value, label):
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        die("%s contains a control or format character" % label)

# Single source of truth for floor-incompatible content reads: the same
# module `agent-plan lint-verify` runs standalone, loaded here the way the
# landing receipt already is, so the admission gate and the lint front door
# can never drift apart.
floor_incompatible_reads = _load(sys.argv[3])["floor_incompatible_reads"]

def clean_rel(value, label):
    if not isinstance(value, str):
        die("%s must be a string" % label)
    reject_controls(value, label)
    value = value.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/") or "."
    if (value.startswith("/") or re.match(r"^[A-Za-z]:", value) or
            (value != "." and any(part in ("", ".", "..") for part in value.split("/")))):
        die("%s must be a normalized repo-relative path" % label)
    return value

def require_id():
    i = env("OMS_ID")
    if not i: die("--id is required for %s" % act)
    if not ID_RE.fullmatch(i): die("--id must match [A-Za-z0-9._-]+")
    return i

def deps_done(d, t):
    return all(d["tasks"].get(x, {}).get("state") == "done" for x in t.get("depends", []))

def issue_lease(t):
    t["lease_epoch"] = int(t.get("lease_epoch", 0)) + 1
    t["lease_id"] = "lease_" + secrets.token_hex(16)

def require_current_lease(t):
    supplied = env("OMS_LEASE_ID")
    current = t.get("lease_id", "")
    if supplied and supplied != current:
        die("task %s lease mismatch; worker is stale" % t["id"])
    if env("OMS_HARNESS_CHILD") == "1" and current and not supplied:
        die("task %s requires --lease-id for harness child mutation" % t["id"])

def owner_id():
    value = env("OMS_AUTOPILOT_OWNER_ID")
    if value and not OWNER_RE.fullmatch(value):
        die("autopilot owner id is invalid")
    return value

def clear_claim(t):
    t.update(state="ready", provider="", ttl="", claimed_at="", reason="",
             lease_id="", autopilot_owner_id="")
    t.pop("claimed_by_participant", None)

def panel_claimant():
    """Room participant of a panel main, so two mains of one provider differ."""
    value = env("OMS_ROOM_PARTICIPANT")
    if env("OMS_PANEL_MAIN_ATTEMPT") and re.fullmatch(r"[A-Za-z0-9._:-]{1,80}", value or ""):
        return value
    return ""

def same_absolute_path(left, right):
    return os.path.normcase(os.path.abspath(left)) == os.path.normcase(os.path.abspath(right))

def is_reparse(info):
    attribute = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(info, "st_file_attributes", 0) & attribute)

def reject_marker_constant(value):
    raise ValueError("non-finite JSON value: %s" % value)

def reject_marker_duplicate_keys(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON key: %s" % key)
        value[key] = item
    return value

def marker_json_is_finite(value):
    if isinstance(value, float):
        return math.isfinite(value)
    if isinstance(value, dict):
        return all(marker_json_is_finite(item) for item in value.values())
    if isinstance(value, list):
        return all(marker_json_is_finite(item) for item in value)
    return True

def worker_marker_dir():
    repo_root = os.path.realpath(env("OMS_REPO"))
    expected = os.path.join(repo_root, ".oms", "delegations")
    supplied = env("OMS_PLAN_MARKERS_DIR") or expected
    marker_dir = os.path.abspath(supplied)
    if not same_absolute_path(marker_dir, expected):
        die("worker marker directory must be the repo-local .oms/delegations directory")
    return marker_dir, expected

def load_worker_markers():
    marker_dir, expected = worker_marker_dir()
    markers = []
    if not os.path.lexists(marker_dir):
        return markers
    try:
        marker_dir_info = os.lstat(marker_dir)
    except OSError as exc:
        die("cannot inspect worker marker directory: %s" % exc)
    if (not stat.S_ISDIR(marker_dir_info.st_mode) or is_reparse(marker_dir_info) or
            not same_absolute_path(os.path.realpath(marker_dir), expected)):
        die("worker marker directory must be a real repo-local directory")
    entries = []
    with os.scandir(marker_dir) as iterator:
        for entry in iterator:
            if len(entries) >= 4096:
                die("worker marker directory exceeds 4096 entries")
            entries.append(entry)
    for entry in entries:
        if not entry.name.endswith(".json"):
            continue
        try:
            # Windows scandir metadata can carry a directory-enumeration inode
            # that is not comparable with the file-id returned by fstat().
            # A pathname lstat and the opened handle share the stable identity
            # contract used by the other bounded readers.
            before = os.lstat(entry.path)
        except OSError as exc:
            die("cannot inspect worker marker %s: %s" % (entry.name, exc))
        if (not stat.S_ISREG(before.st_mode) or is_reparse(before) or
                before.st_size > 64 * 1024):
            die("worker marker %s is not a bounded regular file" % entry.name)
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
        try:
            descriptor = os.open(entry.path, flags)
        except OSError as exc:
            die("cannot open worker marker %s safely: %s" % (entry.name, exc))
        try:
            opened = os.fstat(descriptor)
            if (not stat.S_ISREG(opened.st_mode) or is_reparse(opened) or
                    (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
                die("worker marker %s changed while opening" % entry.name)
            payload = os.read(descriptor, 64 * 1024 + 1)
            if len(payload) > 64 * 1024 or os.read(descriptor, 1):
                die("worker marker %s exceeds 64 KiB" % entry.name)
        finally:
            os.close(descriptor)
        try:
            marker = json.loads(
                payload.decode("utf-8"),
                parse_constant=reject_marker_constant,
                object_pairs_hook=reject_marker_duplicate_keys,
            )
        except (UnicodeError, ValueError):
            die("worker marker %s is malformed or unproven" % entry.name)
        if not isinstance(marker, dict) or not marker_json_is_finite(marker):
            die("worker marker %s is malformed or unproven" % entry.name)
        markers.append(marker)
    return markers

def marker_pid_alive(marker):
    if not persisted_native_pid_is_proven(
        marker.get("native_pid"), marker.get("native_pid_source")
    ):
        # An unproven persisted Win32 pid cannot authorize recovery. Typed
        # recovery paths report it as unproven before reaching this fallback.
        return True
    return process_pid_alive(
        marker.get("pid"), native_pid=marker.get("native_pid")
    )

def exact_lease_markers(markers, task_id, lease):
    return [marker for marker in markers
            if marker.get("task_id") == task_id and marker.get("lease_id") == lease]

def worker_marker_is_typed(marker):
    schema = marker.get("schema")
    if (isinstance(schema, bool) or not isinstance(schema, int)
            or schema not in {1, 2, 3, 4}):
        return False
    marker_id = marker.get("id")
    if (not isinstance(marker_id, str) or not ID_RE.fullmatch(marker_id)
            or marker_id in {".", ".."}):
        return False
    pid = marker.get("pid")
    if (isinstance(pid, bool) or not isinstance(pid, int)
            or pid <= 0 or pid > 0x7FFFFFFF):
        return False
    native_pid = marker.get("native_pid")
    if schema == 4 and "native_pid" not in marker:
        return False
    if "native_pid" in marker and (
            isinstance(native_pid, bool) or not isinstance(native_pid, int)
            or native_pid <= 0 or native_pid > 0xFFFFFFFF):
        return False
    if not persisted_native_pid_is_proven(
            native_pid, marker.get("native_pid_source")):
        return False
    for key in ("task_id", "lease_id", "executor_id"):
        value = marker.get(key, "")
        if (not isinstance(value, str)
                or (value and not ID_RE.fullmatch(value))):
            return False
    if marker.get("executor_id", "") in {".", ".."}:
        return False
    marker_owner = marker.get("autopilot_owner_id", "")
    if (not isinstance(marker_owner, str)
            or (marker_owner and not OWNER_RE.fullmatch(marker_owner))):
        return False
    return True

CLAIM_TTL = int(os.environ.get("OMS_CLAIM_TTL") or 3600)
raw_review_ttl = os.environ.get("OMS_PLAN_REVIEW_TTL", "86400")
REVIEW_TTL = int(raw_review_ttl) if raw_review_ttl.isdigit() else 86400

def parse_ts(s):
    try:
        return datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ")
    except Exception:
        return None

now_dt = parse_ts(ts)

def claim_anchor(t):
    """When this task's TTL clock last restarted. touch refreshes claimed_at,
    so the clock runs from the last heartbeat, not from the claim. review ages
    from when it entered review instead: it waits on a reviewer, not a worker."""
    if t.get("state") == "review":
        return parse_ts(t.get("updated", ""))
    return parse_ts(t.get("claimed_at", "")) or parse_ts(t.get("updated", ""))

def claim_ttl_for(t, default_ttl=CLAIM_TTL):
    v = t.get("ttl", "")
    if isinstance(v, str) and v.isdigit():
        return int(v)
    return default_ttl

def claim_age(t):
    anchor = claim_anchor(t)
    if anchor is None or now_dt is None:
        return None
    return int((now_dt - anchor).total_seconds())

def claim_expired(t):
    """Read-time view of a claim: past its TTL it belongs to a worker nobody
    has heard from, so it is not a live hold on the task. Reads present that
    and change nothing (the same way agent-thread treats an expired CURRENT
    pointer); reclaim is what rewrites the row."""
    if t.get("state") != "claimed":
        return False
    age = claim_age(t)
    if age is None:
        return False
    return age >= claim_ttl_for(t)

def review_expired(t):
    if t.get("state") != "review":
        return False
    age = claim_age(t)
    return age is not None and age >= REVIEW_TTL

def actionable(d, t):
    """Claimable right now: ready, or held by an expired claim."""
    return deps_done(d, t) and (t["state"] == "ready" or claim_expired(t))

def project_contract_verdict(d):
    contract = d.get("project_contract")
    project = project_state_snapshot(os.path.join(env("OMS_REPO"), "PROJECT.md"))
    if contract is None:
        return {
            "bound": False, "satisfied": True,
            "project_state": project.get("state", "invalid"),
            "project_present": project.get("present") is True,
            "project_healthy": project.get("healthy") is True,
            "blocker": "", "expected_spec_sha256": "",
            "current_spec_sha256": project.get("sha256", "")
            if project.get("healthy") is True else "",
        }
    expected = contract.get("spec_sha256", "") if isinstance(contract, dict) else ""
    valid_contract = (
        isinstance(contract, dict) and contract.get("schema") == 1 and
        isinstance(expected, str) and
        re.fullmatch(r"[0-9a-f]{64}", expected) is not None
    )
    current = project.get("sha256", "") if project.get("healthy") is True else ""
    if not valid_contract:
        blocker = "invalid-contract"
    elif project.get("healthy") is not True:
        blocker = "project-unreadable"
    elif project.get("state") == "missing":
        blocker = "project-missing"
    elif project.get("state") == "draft":
        blocker = "project-draft"
    elif project.get("state") not in ("confirmed", "legacy-active"):
        blocker = "project-invalid"
    elif current != expected:
        blocker = "project-drift"
    else:
        blocker = ""
    return {
        "bound": True, "satisfied": blocker == "",
        "project_state": project.get("state", "invalid"),
        "project_present": project.get("present") is True,
        "project_healthy": project.get("healthy") is True,
        "blocker": blocker,
        "expected_spec_sha256": expected if valid_contract else "",
        "current_spec_sha256": current,
    }

def contract_actionable(d, t):
    return (not (t.get("executor_id") or t.get("executor_soul_sha256"))
            and CONTRACT_VERDICT["satisfied"] and actionable(d, t))

def require_project_contract_authority():
    if CONTRACT_VERDICT["satisfied"]:
        return
    die("PROJECT.md contract blocks new task authority: %s" %
        CONTRACT_VERDICT["blocker"])

def expiry_note(t):
    return "claim EXPIRED (age %ss >= ttl %ss, was @%s)" % (
        claim_age(t), claim_ttl_for(t), t.get("provider", "") or "?")

def brief_text(t):
    state = t["state"]
    if claim_expired(t):
        state = "%s [%s; claimable]" % (state, expiry_note(t))
    lines = ["# Task %s: %s" % (t["id"], t["title"]), "state: %s" % state]
    lines.append("depends: %s" % (", ".join(t.get("depends", [])) or "(none)"))
    lines.append("allowed_paths: %s" % (", ".join(t.get("allowed_paths", [])) or "(unrestricted)"))
    if t.get("forbidden_paths"):
        lines.append("forbidden_paths: %s" % ", ".join(t["forbidden_paths"]))
    lines.append("verify: %s" % (t.get("verify") or "(none)"))
    if t.get("assignment"):
        lines.append("assignment: %s" % json.dumps(t["assignment"], sort_keys=True))
    if t.get("role"):
        lines.append("role: %s" % t["role"])
    return "\n".join(lines)

read_only_actions = {"show", "evidence-snapshot", "list", "ready", "status", "next", "brief"}
needs_retirement_guard = act not in read_only_actions
if act == "next" and env("OMS_CLAIM") == "1":
    needs_retirement_guard = True
if act in {"recover-lease", "recover-owner"} and env("OMS_CHECK_ONLY") == "1":
    needs_retirement_guard = False
canonical_plan = os.path.join(os.path.realpath(env("OMS_REPO")), ".oms", "plan", "tasks.json")
if not same_absolute_path(path, canonical_plan):
    needs_retirement_guard = False
if needs_retirement_guard:
    retirement = _load(sys.argv[5])
    retirement_context = {
        "path": path, "ts": ts, "states": STATES, "id_re": ID_RE,
        "die": die, "load_worker_markers": load_worker_markers,
        "worker_marker_is_typed": worker_marker_is_typed,
        "marker_pid_alive": marker_pid_alive,
        "durable_jsonl": os.path.join(os.path.dirname(sys.argv[5]), "durable-jsonl.py"),
    }
    if act == "retire":
        retirement["run"](retirement_context)
        sys.exit(0)
    retirement["cleanup_completed_retirement"](retirement_context)
if act == "accept":
    sys.exit(0)

d = load()
tasks = d["tasks"]
CONTRACT_VERDICT = project_contract_verdict(d)

if act == "init":
    d = {"schema": SCHEMA, "plan_id": "plan_" + secrets.token_hex(16),
         "goal": env("OMS_GOAL"), "accept": env("OMS_ACCEPT"), "tasks": {}}
    save(d); print("plan: initialized (%s)" % path); sys.exit(0)

if act == "ensure-lineage":
    if not os.path.exists(path):
        die("no plan at %s; initialize it before ensuring lineage" % path)
    before = d.get("plan_id")
    value = ensure_plan_id(d)
    if before is None:
        save(d)
        print("plan: lineage initialized (%s)" % value)
    else:
        print("plan: lineage already initialized (%s)" % value)
    sys.exit(0)

if act == "apply-proposal":
    def sha256_file(filename):
        digest = hashlib.sha256()
        with open(filename, "rb") as handle:
            while True:
                chunk = handle.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
        return digest.hexdigest()

    envelope = [clean_rel(item, "allowed envelope")
                for item in split_list(env("OMS_ALLOWED_ENVELOPE"))]
    if not envelope:
        die("allowed envelope is empty")

    def inside_envelope(value):
        candidate = clean_rel(value, "proposal allowed path")
        return within_envelope(candidate, envelope)

    proposal_bytes = read_regular_bytes(proposal_path, "proposal", 1024 * 1024)
    expected_proposal = env("OMS_EXPECTED_PROPOSAL_SHA256")
    if hashlib.sha256(proposal_bytes).hexdigest() != expected_proposal:
        die("proposal bytes changed after review")
    try:
        proposal = json.loads(proposal_bytes.decode("utf-8"))
    except (UnicodeError, ValueError) as exc:
        die("cannot read proposal: %s" % exc)
    if not isinstance(proposal, dict) or proposal.get("schema") != 1:
        die("proposal has an unsupported schema")
    if proposal.get("kind") != "agent-plan-proposal":
        die("proposal kind is not agent-plan-proposal")
    top_keys = {
        "schema", "kind", "spec_sha256", "plan_sha256", "base_sha",
        "id_prefix", "allowed_envelope", "acceptance_files", "tasks",
    }
    if set(proposal) != top_keys:
        die("proposal fields do not match the exact reviewed schema")

    proposal_spec = proposal.get("spec_sha256")
    proposal_plan = proposal.get("plan_sha256")
    proposal_base = proposal.get("base_sha")
    proposal_prefix = proposal.get("id_prefix")
    proposal_envelope = proposal.get("allowed_envelope")
    proposal_acceptance_files = proposal.get("acceptance_files")
    if (not isinstance(proposal_spec, str) or len(proposal_spec) != 64 or
            any(ch not in "0123456789abcdef" for ch in proposal_spec)):
        die("proposal spec_sha256 is invalid")
    if (not isinstance(proposal_base, str) or len(proposal_base) not in (40, 64) or
            any(ch not in "0123456789abcdef" for ch in proposal_base)):
        die("proposal base_sha is invalid")
    if proposal_plan != env("OMS_EXPECTED_PLAN_SHA256"):
        die("proposal plan digest does not match the apply CAS")
    if not isinstance(proposal_prefix, str) or (proposal_prefix and not ID_RE.fullmatch(proposal_prefix)):
        die("proposal id prefix is invalid")
    if not isinstance(proposal_envelope, list) or any(not isinstance(x, str) for x in proposal_envelope):
        die("proposal allowed envelope is invalid")
    reviewed_envelope = sorted(set(
        clean_rel(item, "proposal allowed envelope") for item in proposal_envelope
    )) or ["."]
    if reviewed_envelope != sorted(set(envelope)):
        die("proposal allowed envelope does not match the apply boundary")

    if (not isinstance(proposal_acceptance_files, list) or
            any(not isinstance(item, str) for item in proposal_acceptance_files)):
        die("proposal acceptance_files must be a list")
    if len(proposal_acceptance_files) > 64:
        die("proposal acceptance_files must contain at most 64 paths")
    if any(len(item.encode("utf-8")) > 240 for item in proposal_acceptance_files):
        die("proposal acceptance file paths must be at most 240 UTF-8 bytes")
    reviewed_acceptance_files = sorted(set(
        clean_rel(item, "proposal acceptance file")
        for item in proposal_acceptance_files
    ))
    supplied_acceptance_files = sorted(set(
        clean_rel(item, "--accept-files")
        for item in env("OMS_ACCEPT_FILES").split(",") if item.strip()
    ))
    if proposal_acceptance_files != reviewed_acceptance_files:
        die("proposal acceptance_files must be sorted and unique")
    if supplied_acceptance_files != reviewed_acceptance_files:
        die("proposal acceptance_files do not match --accept-files")

    repo_root = os.path.realpath(env("OMS_REPO"))
    acceptance_manifest = []
    for item in reviewed_acceptance_files:
        if item == ".":
            die("acceptance file must name a regular file, not the repository root")
        filename = os.path.join(repo_root, *item.split("/"))
        if os.path.realpath(filename) != filename:
            die("acceptance file %s must not cross a symlink boundary" % item)
        try:
            info = os.lstat(filename)
        except OSError as exc:
            die("cannot inspect acceptance file %s: %s" % (item, exc))
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            die("acceptance file %s must be a regular non-symlink file" % item)
        value = hashlib.sha256(read_regular_bytes(
            filename, "acceptance file %s" % item, 8 * 1024 * 1024
        )).hexdigest()
        acceptance_manifest.append({"path": item, "sha256": value})

    spec_path = os.path.join(env("OMS_REPO"), "PROJECT.md")
    spec_bytes = read_regular_bytes(spec_path, "PROJECT.md", 1024 * 1024)
    if hashlib.sha256(spec_bytes).hexdigest() != proposal_spec:
        die("PROJECT.md changed after proposal review")
    try:
        spec_bytes.decode("utf-8")
    except UnicodeError:
        die("PROJECT.md must be UTF-8")
    raw_tasks = proposal.get("tasks")
    max_tasks = int(env("OMS_MAX_TASKS") or "12")
    if not isinstance(raw_tasks, list) or not raw_tasks or len(raw_tasks) > max_tasks:
        die("proposal tasks must contain 1..%d entries" % max_tasks)

    prepared = []
    seen = []
    for index, raw in enumerate(raw_tasks):
        if not isinstance(raw, dict):
            die("proposal task %d must be an object" % index)
        if set(raw) - {"assignment"} != {"id", "title", "allowed", "verify", "depends"}:
            die("proposal task %d fields do not match the exact reviewed schema" % index)
        try:
            assignment = validate_assignment(raw.get("assignment", {}))
        except ValueError as exc:
            die(str(exc))
        task_id = raw.get("id")
        title = raw.get("title")
        verify = raw.get("verify")
        if not isinstance(task_id, str) or not ID_RE.fullmatch(task_id):
            die("proposal task %d has an invalid id" % index)
        if task_id in seen:
            die("proposal task ids must be unique")
        if not isinstance(title, str) or not title.strip():
            die("proposal task %s has no title" % task_id)
        reject_controls(title, "proposal task %s title" % task_id)
        if not isinstance(verify, str) or not verify.strip():
            die("proposal task %s has no verify command" % task_id)
        reject_controls(verify, "proposal task %s verify" % task_id)
        if proposal_prefix and not task_id.startswith(proposal_prefix):
            die("proposal task %s does not match id prefix %s" % (task_id, proposal_prefix))
        allowed = raw.get("allowed")
        if not isinstance(allowed, list) or not allowed:
            die("proposal task %s has no allowed paths" % task_id)
        cleaned_allowed = [clean_rel(item, "proposal task %s allowed path" % task_id)
                           for item in allowed]
        if not all(inside_envelope(item) for item in cleaned_allowed):
            die("proposal task %s widens the allowed path envelope" % task_id)
        floor_hits = floor_incompatible_reads(verify, cleaned_allowed)
        if floor_hits:
            die("floor_incompatible_verifier: proposal task %s verify reads %s"
                " (via %s), a file the task itself modifies — the base floor"
                " restores that file from HEAD, so this check can never pass;"
                " verify by executing the restored file (run the suite), not"
                " by reading its content" % (task_id, floor_hits[0][0], floor_hits[0][1]))
        forbidden = raw.get("forbidden") or []
        if not isinstance(forbidden, list):
            die("proposal task %s forbidden paths must be a list" % task_id)
        cleaned_forbidden = [clean_rel(item, "proposal task %s forbidden path" % task_id)
                             for item in forbidden]
        depends = raw.get("depends") or []
        if (not isinstance(depends, list) or any(not isinstance(dep, str) for dep in depends)
                or any(not ID_RE.fullmatch(dep) for dep in depends)
                or len(depends) != len(set(depends))):
            die("proposal task %s dependencies must be ids" % task_id)
        for dep in depends:
            if dep in seen:
                continue
            if dep not in tasks:
                die("proposal task %s depends on unknown/later task %s" % (task_id, dep))
            if tasks[dep].get("state") != "done":
                die("proposal task %s depends on unfinished existing task %s" % (task_id, dep))
        role = raw.get("role") or ""
        if not isinstance(role, str):
            die("proposal task %s role must be a string" % task_id)
        reject_controls(role, "proposal task %s role" % task_id)
        prepared.append({
            "id": task_id, "title": title.strip(), "depends": list(depends),
            "allowed_paths": cleaned_allowed, "forbidden_paths": cleaned_forbidden,
            "verify": verify, "role": role, "assignment": assignment,
        })
        seen.append(task_id)

    contract = d.get("project_contract")
    if contract is not None:
        if (not isinstance(contract, dict) or contract.get("schema") != 1 or
                contract.get("spec_sha256") != proposal_spec or
                contract.get("allowed_envelope") != reviewed_envelope or
                contract.get("acceptance_files", []) != reviewed_acceptance_files or
                contract.get("acceptance_manifest", []) != acceptance_manifest):
            die("existing plan project contract does not match the reviewed proposal")

    immutable = ("id", "title", "depends", "allowed_paths", "forbidden_paths", "verify", "role")
    already = [item["id"] in tasks for item in prepared]
    if any(already):
        if not all(already):
            die("proposal is partially present; refusing a non-atomic recovery")
        for item in prepared:
            current = tasks[item["id"]]
            if current.get("assignment", {}) != item["assignment"]:
                die("existing task %s assignment does not match the reviewed proposal" % item["id"])
            if any(current.get(name, "" if name in ("verify", "role") else []) != item[name]
                   for name in immutable):
                die("existing task %s does not match the reviewed proposal" % item["id"])
        if env("OMS_GOAL") and d.get("goal", "") != env("OMS_GOAL"):
            die("existing plan goal does not match proposal replay")
        if env("OMS_ACCEPT") and d.get("accept", "") != env("OMS_ACCEPT"):
            die("existing plan acceptance does not match proposal replay")
        # A HEAD-relaxed replay is read-only and is allowed only after the
        # reviewed project contract was already persisted by the first apply.
        if contract is not None:
            print("plan: proposal already applied (%s)" % ",".join(seen))
            sys.exit(0)

    try:
        current_head = subprocess.check_output(
            ["git", "-C", env("OMS_REPO"), "rev-parse", "HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode("ascii").strip()
    except (OSError, subprocess.CalledProcessError, UnicodeError):
        die("cannot resolve the repository HEAD")
    if current_head != proposal_base:
        die("repository HEAD changed after proposal review")

    expected_plan = env("OMS_EXPECTED_PLAN_SHA256")
    if os.path.exists(path):
        actual_plan = sha256_file(path)
        if expected_plan != actual_plan:
            die("plan changed after proposal review")
        if env("OMS_GOAL") and d.get("goal", "") != env("OMS_GOAL"):
            die("existing plan goal changed after proposal review")
        if env("OMS_ACCEPT") and d.get("accept", "") != env("OMS_ACCEPT"):
            die("existing plan acceptance changed after proposal review")
    else:
        if expected_plan != "absent":
            die("proposal expected an existing plan")
        if not env("OMS_GOAL") or not env("OMS_ACCEPT"):
            die("initial proposal apply requires --goal and --accept")
        d = {
            "schema": SCHEMA,
            "plan_id": "plan_" + secrets.token_hex(16),
            "goal": env("OMS_GOAL"),
            "accept": env("OMS_ACCEPT"),
            "project_contract": {
                "schema": 1,
                "spec_sha256": proposal_spec,
                "allowed_envelope": reviewed_envelope,
                "acceptance_files": reviewed_acceptance_files,
                "acceptance_manifest": acceptance_manifest,
            },
            "tasks": {},
        }
        tasks = d["tasks"]

    if contract is None:
        # Adoption of an older manual plan is a first apply, not a replay: it
        # stays behind the proposal's HEAD + plan CAS and admits no extra task.
        if tasks:
            proposed_ids = set(item["id"] for item in prepared)
            extra_ids = sorted(set(tasks) - proposed_ids)
            if extra_ids:
                die("legacy plan has task(s) absent from the reviewed proposal: %s" %
                    ", ".join(extra_ids))
            for current_id, current_task in tasks.items():
                current_allowed = current_task.get("allowed_paths") or []
                if not current_allowed or not all(inside_envelope(item) for item in current_allowed):
                    die("existing task %s is outside the reviewed project contract" % current_id)
        d["project_contract"] = {
            "schema": 1,
            "spec_sha256": proposal_spec,
            "allowed_envelope": reviewed_envelope,
            "acceptance_files": reviewed_acceptance_files,
            "acceptance_manifest": acceptance_manifest,
        }
        ensure_plan_id(d)
        if prepared and all(item["id"] in tasks for item in prepared):
            save(d)
            print("plan: proposal already applied (%s)" % ",".join(seen))
            sys.exit(0)

    for item in prepared:
        task_id = item["id"]
        tasks[task_id] = {
            "id": task_id, "title": item["title"], "state": "ready",
            "depends": item["depends"], "allowed_paths": item["allowed_paths"],
            "forbidden_paths": item["forbidden_paths"], "verify": item["verify"],
            "role": item["role"], "assignment": item["assignment"], "provider": "", "ttl": "", "artifact": "",
            "patch": "", "reason": "", "executor_id": "",
            "executor_soul_sha256": "", "lease_epoch": 0, "lease_id": "",
            "autopilot_owner_id": "",
            "review_lease_id": "", "repair_count": 0, "repair_artifact": "",
            "created": ts, "updated": ts,
        }
    save(d)
    print("plan: proposal applied %s" % ",".join(seen))
    sys.exit(0)

if act == "add":
    try:
        assignment = validate_assignment(json.loads(env("OMS_TASK_ASSIGNMENT")))
    except (ValueError, TypeError) as exc:
        die(str(exc))
    i = require_id(); title = env("OMS_TITLE")
    if not title: die("--title is required for add")
    reject_controls(title, "task title")
    reject_controls(env("OMS_VERIFY"), "task verify")
    reject_controls(env("OMS_ROLE"), "task role")
    if d.get("project_contract") is not None:
        die("contract-bound plans accept new tasks only through a reviewed proposal")
    if i in tasks: die("task already exists: %s" % i)
    depends = split_list(env("OMS_DEPENDS"))
    unknown = [x for x in depends if x not in tasks]
    if unknown: die("unknown dependency id(s): %s" % ", ".join(unknown))
    tasks[i] = {
        "id": i, "title": title, "state": "ready",
        "depends": depends,
        "allowed_paths": split_list(env("OMS_ALLOWED")),
        "forbidden_paths": split_list(env("OMS_FORBIDDEN")),
        "verify": env("OMS_VERIFY"),
        "role": env("OMS_ROLE"), "assignment": assignment,
        "provider": "", "ttl": "", "artifact": "", "patch": "", "reason": "",
        "executor_id": "", "executor_soul_sha256": "",
        "autopilot_owner_id": "",
        "lease_epoch": 0, "lease_id": "", "review_lease_id": "", "repair_count": 0,
        "repair_artifact": "",
        "created": ts, "updated": ts,
    }
    save(d); print("plan: added %s (%s)" % (i, title)); sys.exit(0)

def get_task(i):
    t = tasks.get(i)
    if not t: die("no such task: %s" % i)
    return t

if act in ("claim", "start", "finish", "review", "repair", "land", "block", "release", "recover-lease", "reopen", "show", "evidence-snapshot", "touch"):
    i = require_id(); t = get_task(i)
    if act in {"claim", "start", "review", "repair", "land", "finish"} and (
            t.get("executor_id") or t.get("executor_soul_sha256")):
        die("Soul executor receipt is retired; preserve the old evidence and create a fresh plan task")
    if act == "touch":
        # Heartbeat: a live worker refreshes claimed_at so reclaim's TTL clock
        # restarts and it is not mistaken for a dead worker mid-run.
        if t["state"] not in ("claimed", "running"):
            die("task %s is %s; only a claimed/running task can be touched" % (i, t["state"]))
        require_current_lease(t)
        t["claimed_at"] = ts
    elif act == "claim":
        require_project_contract_authority()
        prov = t.get("assignment", {}).get("provider", env("OMS_PROVIDER"))
        if not prov: die("--provider is required for claim")
        # Only a ready task can be claimed; a blocked task must be reopened first.
        if t["state"] != "ready":
            die("task %s is %s; only a ready task can be claimed (reopen blocked first)" % (i, t["state"]))
        if not deps_done(d, t):
            pending = [x for x in t["depends"] if tasks.get(x, {}).get("state") != "done"]
            die("task %s has unfinished dependencies: %s" % (i, ", ".join(pending)))
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="", repair_artifact="",
                 autopilot_owner_id=owner_id())
        who = panel_claimant()
        if who: t["claimed_by_participant"] = who
        else: t.pop("claimed_by_participant", None)
    elif act == "start":
        if t["state"] != "claimed": die("task %s is %s; claim it first" % (i, t["state"]))
        require_current_lease(t)
        t["state"] = "running"
    elif act == "review":
        if t["state"] not in ("claimed", "running"):
            die("task %s is %s; only a claimed/running task can go to review" % (i, t["state"]))
        require_current_lease(t)
        executor_id = env("OMS_EXECUTOR_ID")
        executor_soul = env("OMS_EXECUTOR_SOUL_SHA256")
        if executor_id or executor_soul:
            die("Soul executor receipts cannot be created; use the task brief and lease")
        t.update(state="review", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""),
                 review_lease_id=t.get("lease_id", ""), executor_id=executor_id,
                 executor_soul_sha256=executor_soul, repair_artifact="")
    elif act == "repair":
        # A landing gate can reject already-reviewed work and ask for one
        # bounded repair. Reuse the exact lease that produced the review so no
        # stale worker or wider claim is created. Keep the prior evidence until
        # the repaired worker publishes a replacement review; if it fails, the
        # caller can still inspect the patch that triggered the repair.
        if t["state"] != "review":
            die("task %s is %s; only reviewed work can enter repair" % (i, t["state"]))
        if not env("OMS_LEASE_ID"):
            die("task %s repair requires the exact review --lease-id" % i)
        require_current_lease(t)
        if not t.get("lease_id") or t.get("review_lease_id", "") != t.get("lease_id", ""):
            die("task %s review patch lease mismatch; repair is stale" % i)
        if not t.get("artifact") or not t.get("patch"):
            die("task %s review is missing artifact/patch evidence" % i)
        repair_count = t.get("repair_count", 0)
        if isinstance(repair_count, bool) or not isinstance(repair_count, int) or repair_count < 0:
            die("task %s has an invalid repair counter" % i)
        if repair_count >= 1:
            die("task %s bounded review repair was already used" % i)
        t.update(state="claimed", claimed_at=ts, reason="", repair_count=1,
                 repair_artifact=env("OMS_ARTIFACT") or t.get("repair_artifact", ""))
    elif act == "land":
        if t["state"] != "review":
            die("task %s is %s; only reviewed work can enter landing" % (i, t["state"]))
        expected_fields = (
            "PATCH", "PATCH_SHA256", "VERIFY", "EXECUTOR_ID",
            "EXECUTOR_SOUL_SHA256", "LEASE_ID",
        )
        missing = [name.lower().replace("_", "-") for name in expected_fields
                   if env("OMS_EXPECTED_REVIEW_%s_SET" % name) != "1"]
        if missing:
            die("task %s land requires the complete expected review receipt: %s" %
                (i, ", ".join(missing)))
        if not env("OMS_LEASE_ID"):
            die("task %s land requires the current --lease-id" % i)
        require_current_lease(t)
        expected_lease = env("OMS_EXPECTED_REVIEW_LEASE_ID")
        if (not expected_lease or expected_lease != env("OMS_LEASE_ID") or
                t.get("review_lease_id", "") != expected_lease or
                t.get("lease_id", "") != expected_lease):
            die("task %s review patch lease mismatch; patch is stale" % i)
        if not t.get("artifact") or not t.get("patch"):
            die("task %s review is missing artifact/patch evidence" % i)
        expected_patch = env("OMS_EXPECTED_REVIEW_PATCH")
        stored_patch = t.get("patch", "")
        if not isinstance(stored_patch, str) or stored_patch != expected_patch:
            die("task %s reviewed patch path changed during admission" % i)
        expected_sha = env("OMS_EXPECTED_REVIEW_PATCH_SHA256")
        if not re.match(r"^[0-9a-f]{64}$", expected_sha):
            die("--expected-review-patch-sha256 must be a lowercase SHA-256")
        patch_path = stored_patch
        if not os.path.isabs(patch_path):
            patch_path = os.path.join(env("OMS_REPO"), patch_path)
        digest = hashlib.sha256()
        try:
            with open(patch_path, "rb") as handle:
                while True:
                    chunk = handle.read(1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
        except (OSError, TypeError) as exc:
            die("task %s reviewed patch cannot be hashed: %s" % (i, exc))
        if digest.hexdigest() != expected_sha:
            die("task %s reviewed patch bytes changed during admission" % i)
        stored_verify = t.get("verify", "")
        if (not isinstance(stored_verify, str) or
                stored_verify.replace("\r", "") != env("OMS_EXPECTED_REVIEW_VERIFY").replace("\r", "")):
            die("task %s verify contract changed during admission" % i)
        expected_executor = env("OMS_EXPECTED_REVIEW_EXECUTOR_ID")
        expected_soul = env("OMS_EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256")
        if bool(expected_executor) != bool(expected_soul):
            die("expected review executor receipt requires both id and soul hash")
        if expected_executor and not ID_RE.fullmatch(expected_executor):
            die("--expected-review-executor-id must match [A-Za-z0-9._-]+")
        if expected_soul and not re.match(r"^[0-9a-f]{64}$", expected_soul):
            die("--expected-review-executor-soul-sha256 must be a lowercase SHA-256")
        if (t.get("executor_id", "") != expected_executor or
                t.get("executor_soul_sha256", "") != expected_soul):
            die("task %s executor review receipt changed during admission" % i)
        t["state"] = "landing"
    elif act == "finish" and env("OMS_LANDED_COMMIT"):
        if t["state"] != "review":
            die("task %s is %s; --landed-commit finish requires review" % (i, t["state"]))
        if not t.get("artifact") or not t.get("patch"):
            die("task %s review is missing artifact/patch evidence" % i)
        require_current_lease(t)
        history = t.get("history", [])
        if not isinstance(history, list):
            die("task %s history must be a list" % i)
        landing = commit_landing_proof(env("OMS_LANDED_COMMIT"))
        allowed = t.get("allowed_paths") or []
        if allowed:
            try:
                scope = path_scope["validate_patterns"](
                    path_scope["normalize"](a, "allowed path") for a in allowed)
            except ValueError as exc:
                die("task %s allowed_paths are invalid: %s" % (i, exc))
            span, changed = landed_range_paths(landing["sha"])
            if not any(path_scope["matches"](p, a) for p in changed for a in scope):
                die("landed commit %s does not land task %s: %s changed none of its allowed_paths (%s); "
                    "changed: %s. Finish against the SHA whose land pushed this task's work"
                    % (landing["sha"], i, span, ", ".join(allowed),
                       ", ".join(changed[:5]) + (" ..." if len(changed) > 5 else "") or "nothing"))
        history.append({"schema": 1, "kind": "finish", "ts": ts,
                        "state": "done", "landing": landing})
        t.update(state="done", landing=landing, history=history)
    elif act == "finish":
        # Done is a landing receipt, not a worker self-report. patch-land owns
        # the review -> landing fence after mechanical admission succeeds.
        if t["state"] != "landing":
            die("task %s is %s; finish only after reviewed work enters landing" % (i, t["state"]))
        require_current_lease(t)
        if env("OMS_EXPECTED_LANDING_RECEIPT_SHA256_SET") != "1":
            die("task %s finish requires --expected-landing-receipt-sha256" % i)
        expected_receipt = env("OMS_EXPECTED_LANDING_RECEIPT_SHA256")
        if not re.fullmatch(r"[0-9a-f]{64}", expected_receipt):
            die("--expected-landing-receipt-sha256 must be a lowercase SHA-256")
        if landing_receipt_digest(t) != expected_receipt:
            die("task %s landing receipt changed; stale finish rejected" % i)
        if env("OMS_REFREEZE_ACCEPTANCE") == "1":
            # Consent-aware manifest refreeze (field finding: a consented,
            # admitted, floor-verified landing that touches acceptance-listed
            # files parked at the next accept because the frozen manifest
            # predates it). Only entries the FENCED patch itself modified are
            # recomputed from the landed tree; every other entry keeps its
            # frozen hash, so out-of-band edits — to other acceptance files,
            # or to these files after this landing — still park. The patch's
            # file list comes from the patch bytes the review fence pinned,
            # never from caller argv. Recompute-from-tree is idempotent, so a
            # --recover replay of this finish converges.
            contract = d.get("project_contract")
            patch_rel = env("OMS_PATCH") or t.get("patch", "")
            repo_root = os.path.realpath(env("OMS_REPO") or os.path.dirname(
                os.path.dirname(os.path.dirname(os.path.abspath(path)))))
            if isinstance(contract, dict) and patch_rel:
                patch_abs = os.path.join(repo_root, *patch_rel.split("/")) \
                    if not os.path.isabs(patch_rel) else patch_rel
                touched = set()
                try:
                    with open(patch_abs, encoding="utf-8", errors="replace") as ph:
                        for line in ph:
                            if line.startswith("+++ b/"):
                                touched.add(line[6:].strip())
                            elif line.startswith("diff --git a/") and " b/" in line:
                                # A `git diff --binary` hunk carries no +++
                                # line, so a binary acceptance file would keep
                                # its stale frozen hash and park the next
                                # accept. The header names the post-image.
                                touched.add(line.rsplit(" b/", 1)[1].strip())
                except OSError:
                    touched = set()
                files = contract.get("acceptance_files") or []
                manifest = contract.get("acceptance_manifest") or []
                refrozen = []
                for index_m, rel in enumerate(files):
                    if rel not in touched or index_m >= len(manifest):
                        continue
                    target = os.path.join(repo_root, *rel.split("/"))
                    digest_new = hashlib.sha256()
                    try:
                        with open(target, "rb") as th:
                            total = 0
                            while True:
                                chunk = th.read(1024 * 1024)
                                if not chunk:
                                    break
                                total += len(chunk)
                                if total > 8 * 1024 * 1024:
                                    die("acceptance file %s exceeds the manifest size cap" % rel)
                                digest_new.update(chunk)
                    except OSError:
                        die("cannot refreeze acceptance file %s" % rel)
                    old_hash = manifest[index_m].get("sha256", "")
                    new_hash = digest_new.hexdigest()
                    if old_hash != new_hash:
                        manifest[index_m] = {"path": rel, "sha256": new_hash}
                        refrozen.append({"path": rel, "old": old_hash, "new": new_hash})
                if refrozen:
                    ledger_path = os.path.join(
                        os.path.dirname(path), "manifest-refreeze.jsonl")
                    row_out = {
                        "schema": 1, "kind": "manifest-refreeze", "task": i,
                        "landing_receipt_sha256": expected_receipt,
                        "entries": refrozen, "ts": ts,
                    }
                    ledger_fd = os.open(
                        ledger_path,
                        os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
                    try:
                        os.write(ledger_fd, (json.dumps(
                            row_out, sort_keys=True,
                            separators=(",", ":")) + "\n").encode("utf-8"))
                        os.fsync(ledger_fd)
                    finally:
                        os.close(ledger_fd)
        t.update(state="done", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""))
    elif act == "block":
        r = env("OMS_REASON")
        if not r: die("--reason is required for block")
        if t["state"] in ("claimed", "running", "review", "landing"):
            require_current_lease(t)
        t.update(state="blocked", reason=r)
    elif act == "release":
        # Requeue a claimed/running task (e.g. the worker died) back to ready.
        if t["state"] not in ("claimed", "running", "review", "landing"):
            die("task %s is %s; only a claimed/running/review/landing task can be released" % (i, t["state"]))
        require_current_lease(t)
        clear_claim(t)
    elif act == "recover-lease":
        supplied = env("OMS_LEASE_ID")
        expected_state = env("OMS_EXPECTED_STATE")
        if not expected_state or expected_state not in STATES:
            die("recover-lease requires a valid --expected-state")
        if (not supplied or t.get("lease_id", "") != supplied or
                t.get("state") != expected_state or
                expected_state not in ("claimed", "running")):
            sys.stderr.write("plan: task %s no longer holds that exact claimed/running state+lease\n" % i)
            sys.exit(3)
        matching = exact_lease_markers(load_worker_markers(), i, supplied)
        # A malformed pid is not proof of death. Any exact live marker is a
        # retry/worker veto even when an older dead marker triggered cleanup.
        if any(not worker_marker_is_typed(marker) for marker in matching):
            sys.stderr.write("plan-recovery-outcome: unproven\n")
            sys.stderr.write("plan: task %s has an unproven exact worker marker\n" % i)
            sys.exit(3)
        if any(marker_pid_alive(marker) for marker in matching):
            sys.stderr.write("plan-recovery-outcome: veto\n")
            sys.stderr.write("plan: task %s still has a live exact worker marker\n" % i)
            sys.exit(3)
        if env("OMS_CHECK_ONLY") == "1":
            print("plan: task %s exact state+lease is recoverable" % i)
            sys.exit(0)
        clear_claim(t)
    elif act == "reopen":
        if t["state"] != "blocked":
            die("task %s is %s; only a blocked task can be reopened" % (i, t["state"]))
        clear_claim(t)
    elif act in ("show", "evidence-snapshot"):
        # Computed on a copy: the stored task keeps exactly the fields its
        # writers put there, and a later save() cannot persist a read's view.
        view = dict(t)
        if isinstance(d.get("project_contract"), dict):
            # Contract metadata is a read-only view for admission consumers. It
            # is deliberately not copied into each stored task.
            view["project_contract"] = d["project_contract"]
        if act == "evidence-snapshot":
            view["plan_id"] = d.get("plan_id", "")
        view["claim_expired"] = claim_expired(t)
        if t.get("state") in ("claimed", "running", "review"):
            age = claim_age(t)
            if age is not None:
                view["claim_age_s"] = age
        print(json.dumps(view, ensure_ascii=False, indent=2)); sys.exit(0)
    t["updated"] = ts
    save(d); print("plan: %s -> %s" % (i, t["state"])); sys.exit(0)

# Read-only queries.
ordered = sorted(tasks.values(), key=lambda t: t.get("created", ""))

if act == "recover-owner":
    recovery_owner = owner_id()
    if not recovery_owner:
        die("recover-owner requires --owner-id")
    markers = load_worker_markers()

    recovered = []
    preserved = []
    for task in ordered:
        if task.get("autopilot_owner_id", "") != recovery_owner:
            continue
        state = task.get("state")
        if state in ("review", "landing"):
            preserved.append(task["id"])
            continue
        if state not in ("claimed", "running"):
            continue
        lease = task.get("lease_id", "")
        if not lease:
            preserved.append(task["id"])
            continue
        matching = exact_lease_markers(markers, task["id"], lease)
        owned = [marker for marker in matching
                 if marker.get("autopilot_owner_id") == recovery_owner]
        if any(not worker_marker_is_typed(marker) for marker in matching):
            preserved.append(task["id"])
            continue
        if any(marker_pid_alive(marker) for marker in matching):
            preserved.append(task["id"])
            continue
        if matching and len(owned) != len(matching):
            preserved.append(task["id"])
            continue
        if state == "running" and not owned:
            preserved.append(task["id"])
            continue
        clear_claim(task)
        task["updated"] = ts
        recovered.append(task["id"])
    if recovered:
        save(d)
    result = {"owner_id": recovery_owner, "recovered": recovered,
              "preserved": preserved}
    if env("OMS_AS_JSON") == "1":
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    else:
        print("plan: recovered %s" % (",".join(recovered) if recovered else "(none)"))
        if preserved:
            print("plan: preserved %s" % ",".join(preserved))
    sys.exit(0)

if act == "reclaim":
    # Dead-worker recovery: claim/next store provider+ttl+claimed_at, and this
    # is the consumer. Only ages out claimed (and, opted in, running) tasks by
    # default. review holds a finished artifact awaiting a reviewer, so TTL
    # expiry there means "waiting on reviewer", not "dead worker" — reclaiming
    # it is a separate opt-in with its own clock (updated = when it entered
    # review) and a longer default TTL, and keeps artifact/patch so the
    # finished work is not lost. The claimed/running default is the same
    # OMS_PLAN_CLAIM_TTL the read paths present expiry with, so this frees
    # exactly the claims that already read as expired.
    raw_ttl = env("OMS_TTL")
    if raw_ttl and not raw_ttl.isdigit():
        die("reclaim --ttl must be an integer number of seconds")
    default_ttl = int(raw_ttl) if raw_ttl else CLAIM_TTL
    review_ttl = int(raw_ttl) if raw_ttl else 86400
    states = {"claimed"}
    if env("OMS_INCLUDE_RUNNING") == "1":
        states.add("running")
    if env("OMS_INCLUDE_REVIEW") == "1":
        states.add("review")
    reclaimed = 0
    for t in ordered:
        if t["state"] not in states:
            continue
        anchor = claim_anchor(t)
        if anchor is None or now_dt is None:
            continue
        if t["state"] == "review":
            ttl_s = review_ttl
        else:
            ttl_s = claim_ttl_for(t, default_ttl)
        age = int((now_dt - anchor).total_seconds())
        if age < ttl_s:
            continue
        prov = t.get("provider", "") or "?"
        was = t["state"]
        clear_claim(t)
        t["updated"] = ts
        reclaimed += 1
        print("plan: reclaimed %s from %s (age %ss > ttl %ss, was @%s)" % (t["id"], was, age, ttl_s, prov))
    if reclaimed:
        save(d)
    print("plan: reclaimed %d task(s)" % reclaimed)
    sys.exit(0)

if act == "brief":
    i = require_id()
    print(brief_text(get_task(i)))
    sys.exit(0)

if act == "next":
    candidates = [t for t in ordered if contract_actionable(d, t)]
    if not candidates:
        if not CONTRACT_VERDICT["satisfied"]:
            sys.stderr.write("plan: PROJECT.md contract blocks new task authority: %s\n" %
                             CONTRACT_VERDICT["blocker"])
        sys.stderr.write("plan: no actionable task\n")
        sys.exit(3)
    t = candidates[0]
    if claim_expired(t):
        sys.stderr.write("plan: %s %s; %s\n" % (
            t["id"], expiry_note(t),
            "re-claiming under a new lease" if env("OMS_CLAIM") == "1"
            else "offered as claimable"))
    if env("OMS_CLAIM") == "1":
        prov = t.get("assignment", {}).get("provider", env("OMS_PROVIDER"))
        if not prov:
            die("--claim requires --provider")
        # A fresh lease is the fence: whatever the previous holder does next
        # fails the lease check instead of racing this worker.
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="", autopilot_owner_id=owner_id())
        who = panel_claimant()
        if who: t["claimed_by_participant"] = who
        else: t.pop("claimed_by_participant", None)
        t["updated"] = ts
        save(d)
    if env("OMS_AS_JSON") == "1":
        view = dict(t)
        view["claim_expired"] = claim_expired(t)
        print(json.dumps(view, ensure_ascii=False, indent=2))
    else:
        print(brief_text(t))
    sys.exit(0)

if act == "ready":
    # Ids only: this output is consumed. The expiry note goes to stderr.
    for t in ordered:
        if not contract_actionable(d, t):
            continue
        if claim_expired(t):
            sys.stderr.write("plan: %s %s; counted as ready\n" % (t["id"], expiry_note(t)))
        print(t["id"])
    sys.exit(0)

if act == "list":
    sf = env("OMS_STATE_FILTER")
    if sf and sf not in STATES: die("unknown --state: %s" % sf)
    if env("OMS_AS_JSON") == "1":
        # The same read view `show` computes per task, for every task in one
        # process: a consumer that needs every task's state (the execution
        # graph's plan facts) was starting one agent-plan per task.
        views = []
        for t in ordered:
            if sf and t["state"] != sf: continue
            view = dict(t)
            view["claim_expired"] = claim_expired(t)
            if t.get("state") in ("claimed", "running", "review"):
                age = claim_age(t)
                if age is not None:
                    view["claim_age_s"] = age
            views.append(view)
        print(json.dumps({"schema": 1, "plan_id": d.get("plan_id", ""),
                          "tasks": views}, ensure_ascii=False, indent=2))
        sys.exit(0)
    for t in ordered:
        if sf and t["state"] != sf: continue
        dep = (" depends=%s" % ",".join(t["depends"])) if t["depends"] else ""
        prov = (" @%s" % t["provider"]) if t.get("provider") else ""
        # The state column keeps saying what is stored; the tag says how that
        # stored state reads now.
        tag = (" EXPIRED(%s)" % expiry_note(t)) if claim_expired(t) else ""
        print("%-10s %-9s %s%s%s%s" % (t["id"], t["state"], t["title"], prov, dep, tag))
    sys.exit(0)

if act == "status":
    by = {}
    for t in tasks.values():
        by[t["state"]] = by.get(t["state"], 0) + 1
    claimable = [t["id"] for t in ordered if contract_actionable(d, t)]
    stale = [{"id": t["id"], "provider": t.get("provider", ""),
              "age_seconds": claim_age(t), "ttl_seconds": claim_ttl_for(t)}
             for t in ordered if claim_expired(t)]
    stale_review = [{"id": t["id"], "provider": t.get("provider", ""),
                     "age_seconds": claim_age(t), "ttl_seconds": REVIEW_TTL}
                    for t in ordered if review_expired(t)]
    if env("OMS_AS_JSON") == "1":
        count = len(tasks)
        print(json.dumps({
            "schema": 1, "present": os.path.exists(path),
            "task_count": count, "nonempty": count > 0,
            "all_done": count > 0 and all(t.get("state") == "done" for t in tasks.values()),
            "has_unfinished": any(t.get("state") != "done" for t in tasks.values()),
            "by_state": by, "actionable": claimable,
            "contract": CONTRACT_VERDICT,
            "stale": stale, "stale_review": stale_review,
        }, ensure_ascii=False, sort_keys=True))
        sys.exit(0)
    if d.get("goal"): print("goal: %s" % d["goal"])
    contract = d.get("project_contract")
    if isinstance(contract, dict) and contract.get("spec_sha256"):
        print("contract: PROJECT.md %s scope=%s" % (
            str(contract["spec_sha256"])[:12],
            ",".join(contract.get("allowed_envelope") or []) or "?",
        ))
    if d.get("accept"):
        print("accept: %s" % d["accept"])
        progress_path = os.path.join(os.path.dirname(path), "progress.jsonl")
        last = None
        try:
            with open(progress_path, encoding="utf-8") as fh:
                for line in fh:
                    try:
                        last = json.loads(line)
                    except Exception:
                        continue
        except OSError:
            pass
        if last:
            print("acceptance: %s at %s (base %s)" % (
                last.get("status", "?"), last.get("ts", "?"),
                str(last.get("base_sha", "?"))[:12]))
    order = ["ready", "claimed", "running", "review", "landing", "blocked", "done"]
    print("tasks: %d  [%s]" % (len(tasks),
        " ".join("%s=%d" % (s, by[s]) for s in order if by.get(s))))
    print("ready now: %s" % (" ".join(claimable) if claimable else "(none)"))
    if not CONTRACT_VERDICT["satisfied"]:
        print("contract blocker: %s" % CONTRACT_VERDICT["blocker"])
    for t in ordered:
        if claim_expired(t):
            print("expired claim %s: %s" % (t["id"], expiry_note(t)))
    blocked = [t for t in ordered if t["state"] == "blocked"]
    for t in blocked:
        print("blocked %s: %s" % (t["id"], t.get("reason", "")))
    waiting = [t["id"] for t in ordered
               if t["state"] == "ready" and not deps_done(d, t)]
    if waiting: print("waiting on deps: %s" % " ".join(waiting))
    sys.exit(0)

die("unhandled action: %s" % act)
