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
import statistics

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
    path = os.path.join(root, relative)
    try:
        if os.path.getsize(path) > MAX_CODE_BYTES:
            return "too-large"
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 16), b""):
                digest.update(block)
        return digest.hexdigest()
    except OSError:
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


def group_summaries(rows, metric):
    """[(group, n, mean, stdev, ci95, seeds)] from (value, row) pairs, plus the count of rows without a seed group.
    The latest row of a (group, seed) wins, as a rerun replaces the run it repeats."""
    from oms_runtime.experiment_run import _bootstrap_ci
    latest, loose = {}, 0
    for value, row in rows:
        group, seed = row.get("group"), row.get("seed")
        if not isinstance(group, str) or not group or isinstance(seed, bool) or not isinstance(seed, int):
            loose += 1
            continue
        latest[(group, seed)] = float(value)
    groups = {}
    for (group, seed), value in latest.items():
        groups.setdefault(group, {})[seed] = value
    out = []
    for group, by_seed in groups.items():
        values = [by_seed[seed] for seed in sorted(by_seed)]
        out.append((group, len(values), statistics.mean(values),
                    statistics.stdev(values) if len(values) > 1 else None,
                    _bootstrap_ci(values, group + metric) if len(values) > 1 else None, sorted(by_seed)))
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
