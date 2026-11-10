"""Metric contract and seed grouping for the experiment ledger.

The contract is a small git-tracked JSON file beside the ledger. It says, per
metric, which way is better and what the number was measured on, so a ranking
never mixes numbers that only share a name. A row records the digest of what
the contract said when the run finished; rows written before a contract
existed carry none and keep working.
"""

import hashlib
import json
import math
import os
import re
import stat
import statistics
import unicodedata

CONTRACT_NAME = "metric-contract.json"
FIELDS = ("unit", "direction", "split", "aggregation", "eval_command")
MAX_METRICS, MAX_FILES, MAX_TEXT, MAX_CODE_BYTES = 50, 20, 200, 8 * 1024 * 1024
SEED_RE = re.compile(r"^(?:--?seed|seed)=(-?\d+)$")


class ContractError(ValueError):
    pass


def contract_path(ledger):
    return os.path.join(os.path.dirname(os.path.abspath(ledger)), CONTRACT_NAME)


def _text(value, name, field):
    if not isinstance(value, str) or len(value) > MAX_TEXT or any(ch < " " or ch == "\x7f" for ch in value):
        raise ContractError("metric %s: %s must be one line of at most %d characters" % (name, field, MAX_TEXT))
    return value


def load_contract(path):
    """{metric: spec} from the contract file; {} when there is none. Raises ContractError when unreadable."""
    if not os.path.isfile(path):
        return {}
    try:
        if os.path.getsize(path) > 65536:
            raise ContractError("larger than 65536 bytes")
        with open(path, encoding="utf-8") as handle:
            doc = json.load(handle)
    except (OSError, ValueError) as error:
        raise ContractError(str(error))
    metrics = doc.get("metrics") if isinstance(doc, dict) else None
    if not isinstance(metrics, dict) or doc.get("schema") != 1:
        raise ContractError('expected {"schema": 1, "metrics": {NAME: {...}}}')
    if len(metrics) > MAX_METRICS:
        raise ContractError("more than %d metrics" % MAX_METRICS)
    out = {}
    for name, spec in metrics.items():
        if not isinstance(spec, dict):
            raise ContractError("metric %s: expected an object" % name)
        clean = {field: _text(spec.get(field, ""), name, field) for field in FIELDS}
        if clean["direction"] not in ("max", "min"):
            raise ContractError('metric %s: direction must be "max" or "min"' % name)
        code = spec.get("eval_code", [])
        if not isinstance(code, list) or len(code) > MAX_FILES:
            raise ContractError("metric %s: eval_code must be a list of at most %d paths" % (name, MAX_FILES))
        for item in code:
            _text(item, name, "eval_code path")
            if not item or os.path.isabs(item) or ".." in item.replace("\\", "/").split("/"):
                raise ContractError("metric %s: eval_code paths are relative to the repository, without .." % name)
        clean["eval_code"] = sorted(set(code))
        out[_text(name, name, "name")] = clean
    return out


def _file_digest(root, relative):
    root = os.path.realpath(root)
    path = os.path.realpath(os.path.join(root, relative))
    try:
        if os.path.commonpath((root, path)) != root:
            return "missing"
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(fd, "rb") as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode):
                return "missing"
            if info.st_size > MAX_CODE_BYTES:
                digest = hashlib.sha256()
                digest.update(handle.read(1 << 16))
                handle.seek(-min(info.st_size, 1 << 16), os.SEEK_END)
                digest.update(handle.read(1 << 16))
                return "sample:%d:%s" % (info.st_size, digest.hexdigest())
            digest = hashlib.sha256()
            remaining = MAX_CODE_BYTES
            while remaining:
                block = handle.read(min(1 << 16, remaining))
                if not block:
                    break
                digest.update(block)
                remaining -= len(block)
            return digest.hexdigest()
    except (OSError, ValueError):
        return "missing"


def metric_record(spec, root):
    """What one row keeps about one metric: the readable fields, the eval code digest and one digest over both."""
    code = {relative: _file_digest(root, relative) for relative in spec["eval_code"]}
    eval_sha = hashlib.sha256(json.dumps(code, sort_keys=True).encode("utf-8")).hexdigest() if code else ""
    body = {field: spec[field] for field in FIELDS}
    body["eval_code"] = code
    digest = hashlib.sha256(json.dumps(body, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()
    record = {field: spec[field] for field in FIELDS if field != "eval_command" and spec[field]}
    if eval_sha:
        record["eval_sha256"] = eval_sha
    record["digest"] = digest
    return record


def row_contract(ledger, root, warn):
    """The `metric_contract` field for a new row, or None. A broken contract warns and records nothing."""
    try:
        contract = load_contract(contract_path(ledger))
    except ContractError as error:
        warn("ledger: metric contract ignored (%s); row recorded without it" % error)
        return None
    return {name: metric_record(spec, root) for name, spec in contract.items()} or None


def seed_and_group(cmd, git_sha, dirty_hash, seed="", group=""):
    """(seed or None, group or None). The seed is the explicit one, else the command's own --seed N / seed=N.
    The group is the explicit label, else a digest of the command without its seed at this commit and diff, so
    runs that differ only by seed share it. No seed, no group: a lone run is not a replicate of anything."""
    found, rest, skip = None, [], False
    for index, word in enumerate(cmd):
        if skip:
            skip = False
            continue
        match = SEED_RE.match(word)
        if match:
            found = int(match.group(1))
            continue
        if word in ("--seed", "-seed") and index + 1 < len(cmd) and re.fullmatch(r"-?\d+", cmd[index + 1]):
            found, skip = int(cmd[index + 1]), True
            continue
        rest.append(word)
    if seed != "":
        found = int(seed)
    if found is None:
        return None, (group or None)
    if not group:
        group = hashlib.sha256(json.dumps([rest, git_sha, dirty_hash]).encode("utf-8")).hexdigest()[:12]
    return found, group


def numeric(value):
    return not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(value)


def row_digest(row, metric):
    recorded = row.get("metric_contract")
    entry = recorded.get(metric) if isinstance(recorded, dict) else None
    digest = entry.get("digest") if isinstance(entry, dict) else None
    return digest if isinstance(digest, str) and digest else None


def differing_fields(row, metric, current):
    recorded = row.get("metric_contract")
    entry = recorded.get(metric) if isinstance(recorded, dict) else None
    if not isinstance(entry, dict):
        return ["no contract recorded"]
    fields = [field for field in ("direction", "split", "unit", "aggregation", "eval_sha256")
              if entry.get(field, "") != current.get(field, "")]
    return fields or ["eval_command"]


def comparable(rows, metric, current):
    """(kept rows, {reason: count}) for the rows that may share one ranking.

    `current` is this metric's record under today's contract, or None when the contract does not declare it.
    With a declared metric only rows measured under that exact contract rank. Without one, rows rank together
    only when they all recorded the same digest or none did."""
    left = {}
    if current is not None:
        kept = []
        for row in rows:
            if row_digest(row[1], metric) == current["digest"]:
                kept.append(row)
            else:
                reason = ", ".join(differing_fields(row[1], metric, current))
                left[reason] = left.get(reason, 0) + 1
        return kept, left
    digests = {}
    for row in rows:
        key = row_digest(row[1], metric) or "no contract recorded"
        digests[key] = digests.get(key, 0) + 1
    if len(digests) <= 1:
        return list(rows), {}
    return [], {("contract " + key[:12] if key != "no contract recorded" else key): count
                for key, count in digests.items()}



IDENTITY_KEYS = {"schema", "capture", "git", "config_sha256", "data_sha256",
                 "config_status", "data_status", "recorded_configs", "recorded_data", "digest"}
HEX64 = re.compile(r"[0-9a-f]{64}\Z")


def _canonical_digest(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                     separators=(",", ":"), allow_nan=False).encode("utf-8")).hexdigest()


def _recorded_hashes(rows, label_key, hash_key, lengths):
    if not isinstance(rows, list) or len(rows) > 128:
        raise ContractError("input identity has an invalid or oversized source list")
    found = {}
    for row in rows:
        if not isinstance(row, dict):
            raise ContractError("input identity has an invalid source row")
        label, digest = row.get(label_key), row.get(hash_key)
        if (not isinstance(label, str) or not label or len(label) > 4096
                or any(unicodedata.category(ch).startswith("C") for ch in label)
                or not isinstance(digest, str) or len(digest) not in lengths
                or not re.fullmatch(r"[0-9a-f]+", digest)):
            raise ContractError("input identity lacks a bounded label or recorded digest")
        if label in found and found[label] != digest:
            raise ContractError("input identity has conflicting duplicate sources")
        found[label] = digest
    return found


def make_input_identity(git, configs, data, expected_configs):
    """Fingerprint pre-command captured metadata, not the data/config contents or their actual use."""
    if not isinstance(git, dict):
        raise ContractError("input identity lacks a git record")
    commit, dirty, diff = git.get("commit_full"), git.get("dirty"), git.get("diff_sha256") or ""
    if (not isinstance(commit, str) or not (re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit)
                                          or commit in ("none", "no-commit"))
            or type(dirty) is not int or not 0 <= dirty <= 10**9
            or not isinstance(diff, str) or (diff and not re.fullmatch(r"[0-9a-f]{16}|[0-9a-f]{64}", diff))):
        raise ContractError("input identity has invalid git metadata")
    if (not isinstance(expected_configs, list) or len(expected_configs) > 128
            or any(not isinstance(x, str) or not x or len(x) > 4096 for x in expected_configs)):
        raise ContractError("input identity has invalid declared config paths")
    cfg = _recorded_hashes(configs, "path", "sha256", (32, 64))
    datasets = _recorded_hashes(data, "name", "manifest_sha256", (64,))
    complete = set(expected_configs) == set(cfg)
    body = {"schema": 1, "capture": "pre-command",
            "git": {"commit": commit, "dirty": dirty, "diff_sha256": diff},
            "config_status": "incomplete" if not complete else "recorded" if cfg else "unrecorded",
            "data_status": "recorded" if datasets else "unrecorded",
            "config_sha256": _canonical_digest(sorted(cfg.items())) if cfg and complete else None,
            "data_sha256": _canonical_digest(sorted(datasets.items())) if datasets else None,
            "recorded_configs": len(cfg), "recorded_data": len(datasets)}
    return dict(body, digest=_canonical_digest(body))


def validate_input_identity(value):
    """Validate recorded consistency. Hashes do not authenticate a local writer."""
    if not isinstance(value, dict) or set(value) != IDENTITY_KEYS:
        raise ContractError("invalid recorded input identity")
    if type(value["schema"]) is not int or value["schema"] != 1 or value["capture"] != "pre-command":
        raise ContractError("unsupported recorded input identity")
    git = value["git"]
    if not isinstance(git, dict) or set(git) != {"commit", "dirty", "diff_sha256"}:
        raise ContractError("invalid recorded input identity git state")
    # Reuse producer validation for git metadata, without opening source files.
    make_input_identity({"commit_full": git["commit"], "dirty": git["dirty"],
                         "diff_sha256": git["diff_sha256"]}, [], [], [])
    for prefix, count_key in (("config", "recorded_configs"), ("data", "recorded_data")):
        status, digest, count = value[prefix + "_status"], value[prefix + "_sha256"], value[count_key]
        allowed = ("recorded", "unrecorded", "incomplete") if prefix == "config" else ("recorded", "unrecorded")
        if (status not in allowed or type(count) is not int or not 0 <= count <= 128
                or (digest is not None and (not isinstance(digest, str) or not HEX64.fullmatch(digest)))
                or status == "recorded" and (digest is None or count == 0)
                or status != "recorded" and digest is not None
                or status == "unrecorded" and count != 0):
            raise ContractError("inconsistent recorded input identity coverage")
    body = {key: value[key] for key in value if key != "digest"}
    if not isinstance(value["digest"], str) or value["digest"] != _canonical_digest(body):
        raise ContractError("recorded input identity digest mismatch")
    return value


def data_comparable(rows):
    """Do not rank different recorded datasets, or recorded against unknown, without --mixed."""
    kept, left, datasets = [], {}, {}
    for pair in rows:
        row = pair[1]
        identity = row.get("input_identity")
        if "input_identity" in row:
            try:
                validate_input_identity(identity)
            except (ContractError, TypeError, ValueError):
                left["invalid input identity"] = left.get("invalid input identity", 0) + 1
                continue
        digest = identity["data_sha256"] if identity else None
        key = "data " + digest if digest else "data unrecorded"
        datasets[key] = datasets.get(key, 0) + 1
        kept.append(pair)
    if len(datasets) > 1:
        for key, count in datasets.items():
            left[key] = left.get(key, 0) + count
        return [], left
    return kept, left


def captured_contract(value):
    """Only already captured metric records may cross the capsule-to-ledger bridge."""
    if value is None:
        return None
    if not isinstance(value, dict) or len(value) > MAX_METRICS:
        raise ContractError("invalid captured metric contract")
    clean = {}
    for name, record in value.items():
        _text(name, "recorded", "name")
        if not isinstance(record, dict) or not set(record) <= {"unit", "direction", "split", "aggregation", "eval_sha256", "digest"}:
            raise ContractError("invalid captured metric record")
        if not isinstance(record.get("digest"), str) or not HEX64.fullmatch(record["digest"]):
            raise ContractError("invalid captured metric digest")
        for key, item in record.items():
            _text(item, "recorded", key)
        if record.get("direction") not in ("min", "max"):
            raise ContractError("invalid captured metric direction")
        if "eval_sha256" in record and not HEX64.fullmatch(record["eval_sha256"]):
            raise ContractError("invalid captured evaluation digest")
        clean[name] = dict(record)
    return clean or None


CAPTURE_KEYS = {"schema", "capsule_id", "inputs", "metric_contract", "seeds"}


def capture_for_ledger(git, configs, data, expected_configs, contract, seeds, capsule_id):
    """An allowlisted pre-command projection; never copies config values or source paths into the ledger."""
    value = {"schema": 1, "capsule_id": capsule_id,
             "inputs": make_input_identity(git, configs, data, expected_configs),
             "metric_contract": captured_contract(contract), "seeds": seeds}
    return decode_capture(json.dumps(value, ensure_ascii=False, allow_nan=False))


def decode_capture(raw):
    if not isinstance(raw, str) or len(raw.encode("utf-8")) > 65536:
        raise ContractError("captured run metadata exceeds its limit")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ContractError("duplicate captured metadata key")
            result[key] = value
        return result
    def reject(_):
        raise ContractError("non-finite captured metadata")
    try:
        value = json.loads(raw, object_pairs_hook=unique, parse_constant=reject)
    except (ValueError, RecursionError):
        raise ContractError("invalid captured run metadata") from None
    if (not isinstance(value, dict) or set(value) != CAPTURE_KEYS
            or type(value["schema"]) is not int or value["schema"] != 1
            or not isinstance(value["capsule_id"], str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", value["capsule_id"])
            or ".." in value["capsule_id"]):
        raise ContractError("invalid captured run envelope")
    validate_input_identity(value["inputs"])
    captured_contract(value["metric_contract"])
    seeds = value["seeds"]
    if (not isinstance(seeds, list) or len(seeds) > 128
            or any(not isinstance(seed, str) or len(seed) > 100 for seed in seeds)):
        raise ContractError("invalid captured seed declarations")
    return value


def attach_capture(row, value):
    """Return a row using pre-command provenance, not the current checkout after execution."""
    result = dict(row)
    identity = value["inputs"]
    git = identity["git"]
    result.update(capsule_id=value["capsule_id"], input_identity=identity,
                  git_sha=git["commit"], dirty=git["dirty"], dirty_hash=git["diff_sha256"])
    result.pop("metric_contract", None)
    if value["metric_contract"] is not None:
        result["metric_contract"] = value["metric_contract"]
    seeds = value["seeds"]
    declared = list(dict.fromkeys(seeds))
    seed, group = seed_and_group(result["cmd"], result["git_sha"], result["dirty_hash"])
    status = "command" if seed is not None else "unrecorded"
    if declared:
        if len(declared) == 1 and re.fullmatch(r"-?[0-9]{1,18}", declared[0]):
            explicit = int(declared[0])
            if seed is not None and seed != explicit:
                seed, group, status = None, None, "conflicting-declarations"
            else:
                seed, group = seed_and_group(result["cmd"], result["git_sha"], result["dirty_hash"], str(explicit))
                status = "declared"
        else:
            seed, group, status = None, None, "multiple-or-unrecognized"
    result.pop("seed", None)
    result.pop("group", None)
    result["seed_status"] = status
    if seed is not None:
        result["seed"] = seed
        result["group"] = group
    return result


def group_summaries(rows, metric):
    """Summarize append-ordered rows; reruns replace the same seed only inside an exact recorded scope.

    Explicit group labels never merge different input identities or metric contracts. Legacy rows retain
    their old label when no conflicting recorded scope exists. Unknown external inputs stay unproven.
    """
    from oms_runtime.experiment_run import _bootstrap_ci
    latest, loose, scopes = {}, 0, {}
    for value, row in rows:
        group, seed = row.get("group"), row.get("seed")
        if (not isinstance(group, str) or not group or len(group) > 60
                or not re.fullmatch(r"[A-Za-z0-9._-]+", group)
                or type(seed) is not int or not numeric(value)):
            loose += 1
            continue
        identity = row.get("input_identity")
        if "input_identity" in row:
            try:
                validate_input_identity(identity)
            except (ContractError, TypeError, ValueError):
                loose += 1
                continue
            if (identity["config_status"] == "incomplete"
                    or identity["git"]["commit"] in ("none", "no-commit")
                    or identity["git"]["dirty"] and not identity["git"]["diff_sha256"]):
                loose += 1
                continue
        signature = (identity["digest"] if identity else None, row_digest(row, metric))
        scopes.setdefault(group, set()).add(signature)
        latest[(group, signature, seed)] = float(value)
    groups = {}
    for (group, signature, seed), value in latest.items():
        groups.setdefault((group, signature), {})[seed] = value
    out = []
    for (group, signature), by_seed in groups.items():
        label = group
        if len(scopes[group]) > 1:
            label = group[:44] + "@" + _canonical_digest(signature)[:12]
        values = [by_seed[seed] for seed in sorted(by_seed)]
        out.append((label, len(values), statistics.mean(values),
                    statistics.stdev(values) if len(values) > 1 else None,
                    _bootstrap_ci(values, label + metric) if len(values) > 1 else None, sorted(by_seed)))
    return out, loose


SPLIT_ROLES = ("selection", "final")


def load_split_roles(path):
    """{split: role} from the contract's optional top-level "splits" block; {} when there is none.
    The block is advisory: it never enters a metric's digest, so adding it keeps every recorded row comparable."""
    if not os.path.isfile(path):
        return {}
    try:
        with open(path, encoding="utf-8") as handle:
            doc = json.load(handle)
    except (OSError, ValueError) as error:
        raise ContractError(str(error))
    splits = doc.get("splits") if isinstance(doc, dict) else None
    if splits is None:
        return {}
    if not isinstance(splits, dict) or len(splits) > MAX_METRICS:
        raise ContractError('"splits" must be an object of at most %d splits' % MAX_METRICS)
    roles = {}
    for name, spec in splits.items():
        role = spec.get("role") if isinstance(spec, dict) else None
        if role not in SPLIT_ROLES:
            raise ContractError('split %s: role must be "selection" or "final"' % _text(name, name, "split name"))
        roles[name] = role
    return roles


def final_split_report(ledger, roles, show=10):
    """Advisory lines on how often runs reported a metric measured on a `final` split. [] without such a split.
    A run counts once however many final-split metrics it reported. A variant is the run's seed group, else the
    command at its commit and diff, so reruns of one configuration are not several variants. The split a row
    measured is the one it recorded; the role is today's contract's."""
    final = {name for name, role in roles.items() if role == "final"}
    if not final:
        return []
    runs, variants, by_group, names = 0, set(), {}, set()
    try:
        handle = open(ledger, encoding="utf-8", errors="replace")
    except OSError:
        return []
    with handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            metrics = row.get("metrics") if isinstance(row, dict) else None
            recorded = row.get("metric_contract") if isinstance(row, dict) else None
            if not isinstance(metrics, dict) or not isinstance(recorded, dict):
                continue
            hit = {recorded[key].get("split") for key, value in metrics.items()
                   if numeric(value) and isinstance(recorded.get(key), dict)
                   and isinstance(recorded[key].get("split"), str)} & final
            if not hit:
                continue
            runs += 1
            names |= hit
            group = row.get("group")
            if isinstance(group, str) and group:
                by_group[group] = by_group.get(group, 0) + 1
                variants.add(group)
            else:
                variants.add(hashlib.sha256(json.dumps([row.get("cmd"), row.get("git_sha"),
                                                         row.get("dirty_hash")]).encode("utf-8")).hexdigest()[:12])
    if not runs:
        return []
    label = ", ".join(sorted(names))
    lines = ["advisory: %d recorded run(s) reported a metric on the final split %s" % (runs, label)]
    for group in sorted(by_group)[:show]:
        lines.append("  seed group %s: %d run(s)" % (group[:60], by_group[group]))
    if len(by_group) > show:
        lines.append("  ... %d more seed group(s)" % (len(by_group) - show))
    if sum(by_group.values()) < runs:
        lines.append("  no seed group: %d run(s)" % (runs - sum(by_group.values())))
    if len(variants) > 1:
        lines.append("advisory: %s %s evaluated %d times across %d variants"
                     % (label, "split" if len(names) == 1 else "splits", runs, len(variants)))
    return lines
