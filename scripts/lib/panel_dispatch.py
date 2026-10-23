"""Worker dispatch, continuation, cancellation and council orchestration."""

from typing import NamedTuple
from pathlib import Path
import dispatch_supervisor as dispatch_store
import hashlib
import io
import json
import os
import re
import room
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROLE_ALIAS_LETTERS = {"builder":"B", "worker":"W", "reviewer":"R", "verifier":"V",
                      "security-auditor":"S", "advisor":"A", "critic":"C", "architect":"H",
                      "documenter":"D", "integrator":"I", "performance-engineer":"P",
                      "release-engineer":"E", "refactorer":"F", "researcher":"X"}

# Call-local facade imports preserve the public entry point and avoid import cycles.
class PlanBinding(NamedTuple):
    plan_id: str
    task_id: str
    lease_id: str
    claim_provider: str
    owner_provider: str
    owner_participant: str
    owner_attempt: str
    room_id: str

    @staticmethod
    def unique_fields(pairs):
        result = {}
        for name, value in pairs:
            if name in result:
                raise ValueError("duplicate plan observation field")
            result[name] = value
        return result

    @classmethod
    def parse(cls, value):
        if not isinstance(value, str) or len(value.encode("utf-8")) > 4096:
            raise ValueError("plan observation exceeds 4096 bytes")
        fields = json.loads(value, object_pairs_hook=cls.unique_fields)
        if not isinstance(fields, dict) or set(fields) != set(cls._fields):
            raise ValueError("plan observation requires exactly the binding fields")
        formats = dict(plan_id=r"plan_[0-9a-f]{32}", lease_id=r"lease_[0-9a-f]{32}",
                       task_id=r"[A-Za-z0-9._-]{1,160}", owner_attempt=r"att_[0-9a-f]{32}",
                       owner_participant=r"[A-Za-z0-9][A-Za-z0-9._:-]{0,79}",
                       room_id=r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}",
                       claim_provider=r"[a-z0-9][a-z0-9._-]{0,63}",
                       owner_provider=r"[a-z0-9][a-z0-9._-]{0,63}")
        for name, pattern in formats.items():
            if not isinstance(fields[name], str) or not re.fullmatch(pattern, fields[name]):
                raise ValueError("invalid plan observation " + name)
        return cls(**fields)

    def validate(self, repo, env, route):
        """Recheck observation under the caller's marker-set and canonical plan fences."""
        from terminal_panel import ROOT, _read, active_mains
        import runpy
        plan = json.loads(_read(repo, ".oms/plan/tasks.json", 4 * 1024 * 1024),
                          object_pairs_hook=self.unique_fields)
        task = plan["tasks"][self.task_id]
        if (plan.get("plan_id") != self.plan_id or task.get("id") != self.task_id
                or task.get("lease_id") != self.lease_id or task.get("state") not in {"claimed", "running"}
                or task.get("provider") != self.claim_provider
                or task.get("claimed_by_participant") != self.owner_participant):
            raise ValueError("plan observation no longer matches its claim")
        if (env.get("OMS_AGENT") != self.owner_provider or env.get("OMS_PANEL_MAIN_ATTEMPT") != self.owner_attempt
                or not room.same_room(repo, env.get("OMS_ROOM_ID"), self.room_id)
                or Path(env.get("OMS_ROOM_REPO", "")).resolve() != repo.resolve()
                or env.get("OMS_TASK_ID") != self.task_id
                or not env.get("OMS_ROOM_PARTICIPANT")
                or env.get("OMS_ROOM_ADMITTED_PARTICIPANT") != env.get("OMS_ROOM_PARTICIPANT")):
            raise ValueError("foreign plan observation caller")
        state = room.project(room.records(repo, self.room_id))
        main = active_mains(repo, self.room_id, state, self.owner_provider,
                            caller=self.owner_participant, attempt=self.owner_attempt).get(self.owner_participant)
        child = room.participant(state, env["OMS_ROOM_PARTICIPANT"])
        if (state["closed"] or not main or main.get("attempt_id") != self.owner_attempt
                or not room.same_room(repo, main.get("refs", {}).get("panel_room_id", self.room_id), self.room_id)
                or main.get("refs", {}).get("panel_room_participant", main.get("attempt_id")) != self.owner_participant
                or not child["joined"] or child["role"] not in {"worker", "researcher"}
                or env.get("OMS_PANEL_ROLE", "worker") != child["role"]
                or child.get("parent") != self.owner_participant
                or child["provider"] != route["provider"] or child.get("model") != route["model"]):
            raise ValueError("plan observation main/room/admitted child mismatch")
        assignment = runpy.run_path(str(ROOT / "scripts/lib/task-assignment.py"))["validate"](task.get("assignment", {}))
        for field in ("provider", "model", "reasoning_effort", "fallback_model"):
            if field in assignment and assignment[field] != route[field]:
                raise ValueError("reviewed assignment mismatch: " + field)


def provider_catalog(selection="all"):
    """Read the shared registry and cached models without executing a CLI."""
    from terminal_panel import ENTRY, PROVIDERS, ROOT, native_adapter
    result = subprocess.run(["bash", str(ENTRY), "models", "--providers", selection, "--json"],
                            capture_output=True, text=True, check=False, timeout=30,
                            stdin=subprocess.DEVNULL)
    if result.returncode:
        raise ValueError("provider selection is unavailable; use a registered provider or oms-agent-adapter-ID")
    rows = json.loads(result.stdout)["providers"]
    access = subprocess.run(["bash", "-c", '''
        . "$1"
        shift
        for provider in "$@"; do
            write=false
            oms_provider_supports_access "$provider" write && write=true
            printf '%s\\t%s\\n' "$provider" "$write"
        done
        ''', "panel-provider-access", str(ROOT / "scripts/lib/provider-registry.sh")] +
        [row["provider"] for row in rows], capture_output=True, text=True, check=False,
        timeout=10, stdin=subprocess.DEVNULL)
    if access.returncode:
        raise ValueError("provider access contract is unavailable")
    writes = dict(line.replace("\r", "").split("\t") for line in access.stdout.splitlines())
    return [dict(row, installed=row["present"], access=["read"] +
                 (["write"] if writes.get(row["provider"]) == "true" else []),
                 native_launch=row["provider"] in PROVIDERS or bool(native_adapter(row["provider"])),
                 session_messaging="room_polling", native_hook_wiring="unverified")
            for row in rows]


def routing_status(repo, room_id=None, main=None):
    """Reuse the board's recorded HUD/pane readings without a provider usage query."""
    from terminal_panel import repository, snapshot
    if not (room_id or os.environ.get("OMS_ROOM_ID")):
        return {}
    from panel_metrics import collect
    try:
        state, _ = snapshot(repository(repo), room_id=room_id)
        return collect(state, main or os.environ.get("OMS_PANEL_MAIN_ATTEMPT"), repo=repository(repo))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return {}


def room_history(repo, owner, task_id=None, candidate=None, room_id=None):
    """Dispatches per provider in this room and the provider that wrote the reviewed work; empty when unknown.

    The writer is bound to linkage, never recency (attempt order follows updated_at, which telemetry advances):
    only the write attempts whose recorded artifact names the candidate's patch path, digest and base. Identical
    bytes may come from several providers, so ambiguous or missing linkage leaves the writer unknown.
    room_id names the room explicitly; without it the owner's environment binding decides."""
    from terminal_panel import child_environment, events
    recent, writers, rows = {}, {}, []
    try:
        room_id = room.resolve(repo, room_id or child_environment(owner, repo).get("OMS_ROOM_ID"))
        if not room_id:
            return None, None
        for attempt in json.loads(events(repo, "list", "--json", output=True)):
            refs = attempt.get("refs", {})
            if not refs.get("panel_alias") or not room.same_room(repo, refs.get("panel_room_id"), room_id):
                continue
            ran = refs.get("panel_provider") or attempt.get("provider")
            recent[ran] = recent.get(ran, 0) + 1
            rows.append(attempt)
            if task_id and attempt.get("task_id") == task_id and refs.get("panel_access") == "write":
                writers[attempt.get("attempt_id")] = ran
        linkage = candidate if isinstance(candidate, dict) else {}
        if not (writers and all(linkage.get(key) for key in ("patch", "sha256", "base"))):
            return recent, None
        import panel_results
        _, artifacts = panel_results._records(repo, _attempts=rows, task_id=task_id)
        owners = {writers[a.get("attempt_id")] for a in artifacts if a.get("attempt_id") in writers
                  and a.get("patch") == linkage["patch"] and a.get("patch_sha256") == linkage["sha256"]
                  and a.get("base_sha") == linkage["base"]}
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        return None, None
    return recent, next(iter(owners)) if len(owners) == 1 else None


def route_inputs(owner, repo=None, room_id=None, main=None, task_id=None, candidate=None):
    """Quota readings, per-provider dispatch counts and reviewed writer that dispatch feeds allocate()."""
    from terminal_panel import routing_status
    repo = repo or os.environ.get("OMS_ROOM_REPO") or os.getcwd()
    return (routing_status(repo, room_id, main),) + room_history(repo, owner, task_id, candidate, room_id)


def selected_route(owner, role, workload, access, purpose, target=None, model=None, effort=None,
                   repo=None, room_id=None, main=None, task_id=None, candidate=None):
    from terminal_panel import MODEL_IDS, PROVIDERS, allocate, newest_route_model, policy, provider_catalog
    from terminal_panel import route_family, routing_status
    if owner not in PROVIDERS:
        owner = provider_catalog(owner)[0]["provider"]
    if effort not in (None, "low", "medium", "high") or role == "researcher" and effort not in (None, "low"):
        raise ValueError("invalid reasoning effort; researchers use low effort")
    if model and not target:
        raise ValueError("explicit peer model requires --to")
    if target and workload:
        raise ValueError("--to is an explicit route; omit the --workload tier")
    if not target or role == "researcher" or target in PROVIDERS and not model:
        # Tier routing reads installed providers from the cached registry; a researcher's --to only narrows it.
        rows = provider_catalog(target or ",".join(sorted({c["provider"] for tier in policy()["tiers"].values()
                                                           for c in tier})))
        readings, recent, writer = ({}, None, None) if target else route_inputs(
            owner, repo, room_id, main, task_id, candidate)
        route = allocate(owner, role, workload, access, purpose,
                         {row["provider"] for row in rows if row["installed"]}, readings, recent, writer)
        peer = next(row for row in rows if row["provider"] == route["provider"])
        route.update(binary=peer["binary"], model=newest_route_model(
            route["provider"], route["model"], peer.get("catalog_models", peer.get("models", []))))
        if model and model != route["model"]:
            raise ValueError("researchers require the newest configured/catalog Luna or Haiku model")
        # A role route keeps its tier's model; the main may raise or lower effort for one harder or simpler subtask.
        return dict(route, effort=effort) if effort else route
    route = allocate(owner, role, None, access, purpose)
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", target) or ".." in target:
        raise ValueError("--to selects one registered provider")
    peer = provider_catalog(target)[0]
    if model is None and not peer["exact_model_override"]:
        model = "provider-default"
    elif (not model or model == "provider-default" or model.startswith("-") or len(model) > 160
            or not re.fullmatch(r"[A-Za-z0-9_./:-]+", model)):
        raise ValueError("--to requires an explicit valid --model for this provider")
    elif not peer["exact_model_override"]:
        raise ValueError("this provider cannot honor an exact --model")
    if access not in peer["access"]:
        raise ValueError("custom write providers require explicit OMS_PROVIDER_WRITE_ADAPTERS opt-in")
    if peer["provider"] == "codex" and model not in MODEL_IDS["codex"] and not (
            route_family("codex", model) and model in peer.get("catalog_models", peer.get("models", []))):
        raise ValueError("Codex workers require a known versioned Sol, Luna or Astra model")
    return dict(route, provider=peer["provider"], binary=peer["binary"], model=model, effort=effort)


def _patch_in_head(repo, body, reverse=True, with_error=False):
    """Check patch applicability on a scratch index rooted at HEAD, optionally in reverse."""
    with tempfile.TemporaryDirectory(prefix="oms-head-has-") as scratch:
        patch_file = Path(scratch) / "previous.patch"
        patch_file.write_bytes(body)
        env = dict(os.environ, GIT_INDEX_FILE=str(Path(scratch) / "index"), GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_CONFIG_SYSTEM=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        git = ["git", "-c", "core.hooksPath=" + os.devnull, "-c", "core.fsmonitor=false", "-C", str(repo)]
        apply = ["apply", "--check"]
        if reverse:
            apply.append("-R")
        apply.extend(["--cached", str(patch_file)])
        try:
            loaded = subprocess.run(git + ["read-tree", "HEAD"], env=env, capture_output=True,
                                    text=True, errors="replace", check=False, stdin=subprocess.DEVNULL, timeout=60)
            checked = (subprocess.run(git + apply, env=env, capture_output=True, text=True, errors="replace",
                                      check=False, stdin=subprocess.DEVNULL, timeout=60)
                       if loaded.returncode == 0 else loaded)
            outcome = checked.returncode == 0
            return (outcome, checked.stderr or checked.stdout) if with_error else outcome
        except (OSError, subprocess.SubprocessError) as error:
            return (False, str(error)) if with_error else False


def _continuation_base_note(repo, base, body, allow_stale_base=False):
    """Check a carried continuation patch against HEAD without changing the repository."""
    try:
        base_result = subprocess.run(["git", "-C", str(repo), "rev-parse", "--verify", base + "^{commit}"],
                                     capture_output=True, text=True, errors="replace", check=False,
                                     stdin=subprocess.DEVNULL, timeout=60)
        current = subprocess.run(["git", "-C", str(repo), "rev-parse", "--verify", "HEAD^{commit}"],
                                 capture_output=True, text=True, errors="replace", check=False,
                                 stdin=subprocess.DEVNULL, timeout=60)
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError("could not resolve continuation base or current HEAD before continuing: %s" % error)
    if base_result.returncode:
        # The base is not in this checkout (another clone or a pruned object): the harness
        # still replays at the original base, so there is no drift to judge here.
        return "recorded continuation base %s is not in this checkout; drift was not checked" % base
    if current.returncode:
        raise ValueError("could not resolve current HEAD before continuing")
    base = base_result.stdout.strip()
    head = current.stdout.strip()
    if head == base:
        return ""
    if allow_stale_base:
        return ("continued patch base %s differs from current HEAD %s; proceeding because "
                "--allow-stale-base was given" % (base, head))
    applies, error = _patch_in_head(repo, body, reverse=False, with_error=True)
    if not applies:
        paths = []
        for line in error.splitlines():
            match = re.match(r"(?:error: )?([^:]+): (?:patch does not apply|already exists in working directory|does not exist in index)", line)
            if match and match.group(1) not in paths:
                paths.append(match.group(1))
        detail = ", ".join(paths) if paths else (error.strip() or "git apply --check failed")[:300]
        raise ValueError("continued patch base %s differs from current HEAD %s and does not apply cleanly (%s); "
                         "refusing continuation. Recover by starting a fresh dispatch from current HEAD and "
                         "including the prior patch as context (for example, --from-task). Use "
                         "--allow-stale-base to keep the original-base continuation behavior." % (base, head, detail))
    return "continued patch base %s differs from current HEAD %s; carried patch applies cleanly" % (base, head)


def _codex_resume_usage_baseline(usage):
    totals = usage.get("session_totals") if isinstance(usage, dict) else None
    if not isinstance(totals, dict):
        return None
    fields = ("input_tokens", "output_tokens", "cached_input_tokens")
    values = [totals.get(key) for key in fields]
    if not all(type(value) is int and value >= 0 for value in values):
        return None
    return json.dumps(dict(zip(fields, values)), separators=(",", ":"))


def _stopped_write_continuation(last, eligible_result):
    refs = last.get("refs", {})
    return (refs.get("panel_access") == "write" and not eligible_result
            and refs.get("panel_location", "").startswith("worktree:"))


def continuation(repo, owner, main, task_id, route, brief, env, work_repo=None, patch_reference=None,
                 allow_stale_base=False):
    """(brief file, native session or None, note) for the next round of one of this main's workers."""
    from terminal_panel import MAX_PROMPT, _rework_history, clean, events, owning_attempt
    from terminal_panel import repository, results, session_stored, verified_patch, work_repository
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", task_id or ""):
        raise ValueError("invalid task ID to continue")
    head_repo = work_repository(work_repo or repo)
    if env.get("OMS_ROOM_REPO"):
        repo = repository(env["OMS_ROOM_REPO"])
    parents = owning_attempt(repo, owner, main, env)
    rows = json.loads(events(repo, "list", "--json", output=True))
    earlier = _rework_history(rows, task_id, parents)
    if not earlier:
        raise ValueError("this main has no worker for task %s to continue" % task_id)

    def historical_worker_exited(row):
        from terminal_panel import _dispatch_call_finished
        from panel_view import TERMINAL_STATES
        parent = next((r for r in rows if r.get("attempt_id") == row.get("parent_attempt_id")), {})
        if parent.get("terminal") is False:
            return True
        if (parent.get("terminal") is not True or row.get("terminal") is not True
                or row.get("state") not in TERMINAL_STATES):
            return False
        refs = row.get("refs", {})
        room_id = refs.get("panel_room_id")
        member = {"participant": refs.get("panel_room_participant"),
                  "parent": parent.get("refs", {}).get("panel_room_participant", parent.get("attempt_id"))}
        if not isinstance(room_id, str) or not room_id or not member["parent"]:
            return False
        try:
            return _dispatch_call_finished(repo, room_id, member)
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            return False

    # A released main does not prove its worker or provider session has stopped.
    if not all(historical_worker_exited(row) for row in earlier):
        raise ValueError("historical worker exit is unconfirmed for task %s; continuation refused" % task_id)
    last = earlier[-1]
    session = last.get("refs", {}).get("native_session")
    found = results(repo, task_id)["rows"]
    call = next((c for c in reversed(found[0].get("calls", []) if found else [])
                 if c.get("attempt_id") == last["attempt_id"]), {})
    if not call:
        # The display slice keeps only the newest calls; later review, admit and
        # land rows must not hide the worker's own row from eligibility.
        import panel_results
        try:
            attempts, indexed = panel_results._records(repo, full=True, task_id=task_id)
            own = [r for r in indexed if r.get("attempt_id") == last["attempt_id"]]
            full = panel_results.results(repo, task_id, _records_data=(attempts, own))["rows"]
            call = next((c for c in reversed(full[0].get("calls", []) if full else [])
                         if c.get("attempt_id") == last["attempt_id"]), {})
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            call = {}
    head = subprocess.run(["git", "-C", str(head_repo), "rev-parse", "--short=12", "HEAD"], capture_output=True,
                          text=True, check=False, stdin=subprocess.DEVNULL).stdout.strip()
    old = last.get("base_sha") or "an earlier commit"
    # A digest proves which bytes were recorded, not that the worker call
    # completed successfully. A plain success or an exact verification failure
    # may expose a prior patch; guard stops and other failures remain salvage-only.
    linked_call = call.get("attempt_id") == last.get("attempt_id")
    successful_call = type(call.get("exit")) is int and call.get("exit") == 0
    # The lifecycle attempt carries the failure code; the indexed call row
    # carries exit=1 and the verifier's own exit. 124 is a verifier timeout,
    # 125 a verifier that never ran and higher codes a signal; none of those
    # proves the checks judged the patch, so they stay salvage-only.
    verified_failure = (linked_call and last.get("reason_code") == "verification_failed"
                        and type(call.get("verify_exit")) is int and 0 < call.get("verify_exit") < 124
                        and not call.get("guard") and bool(call.get("patch")))
    if verified_failure:
        try:
            verified_patch(repo, call)
        except (OSError, ValueError, TypeError):
            verified_failure = False
    verification_section = ""
    unsafe_verification_path = False
    if verified_failure and call.get("reason_code") != "worker_authority_violation":
        command = "verification command unavailable"
        output = "verification output unavailable"
        try:
            import panel_results
            import terminal_panel
            parent = next((row for row in rows if row.get("attempt_id") == last.get("parent_attempt_id")), {})
            refs = last.get("refs", {})
            room_id = refs.get("panel_room_id")
            member = refs.get("panel_room_participant")
            caller = parent.get("refs", {}).get("panel_room_participant", parent.get("attempt_id"))
            verify_command = (_dispatch_verifier(repo, room_id, member, caller, task_id=task_id)
                              if isinstance(room_id, str) and isinstance(member, str)
                              and isinstance(caller, str) and caller else None)

            def outbound_clean(value):
                with tempfile.NamedTemporaryFile("w", encoding="utf-8") as candidate:
                    candidate.write(value)
                    candidate.flush()
                    checked = subprocess.run(
                        ["bash", "-c", '. "$1"; ma_validate_outbound_prompt "$2"',
                         "oms-continuation-scan", str(terminal_panel.OUTBOUND_SCANNER), candidate.name],
                        capture_output=True, text=True, check=False)
                return checked.returncode == 0

            _, artifacts = panel_results._records(repo, _attempts=rows, task_id=task_id)
            record = next((row for row in reversed(artifacts)
                           if row.get("attempt_id") == last["attempt_id"]
                           and row.get("kind") == "delegate"
                           and row.get("event_id") == call.get("event_id")
                           and row.get("artifact")), None)
            if record:
                artifact = record.get("artifact")
                if isinstance(artifact, str) and artifact.endswith(".md"):
                    sidecar = artifact[:-3] + ".verify.txt"
                    raw = panel_results._read(repo, sidecar, 8192, missing=True).decode("utf-8", errors="replace")
                    if raw:
                        try:
                            import panel_review
                            raw = panel_review._relativize_quoted_paths(raw, [str(Path(repo).resolve())])
                        except RuntimeError:
                            unsafe_verification_path = True
                    if raw and panel_results._contains_outbound_machine_path(raw):
                        unsafe_verification_path = True
                    elif raw and outbound_clean(raw):
                        encoded = raw.encode("utf-8")
                        if len(encoded) > 4000:
                            encoded = encoded[-4000:]
                            encoded = encoded[encoded.find(b"\n") + 1:] if b"\n" in encoded else b""
                        output = encoded.decode("utf-8", errors="replace") or "verification output unavailable"

            if (isinstance(verify_command, str) and verify_command.strip()
                    and not panel_results._contains_outbound_machine_path(verify_command)
                    and outbound_clean(verify_command)):
                command = verify_command
        except (OSError, ValueError, KeyError, TypeError, UnicodeError, subprocess.SubprocessError):
            pass
        if not unsafe_verification_path:
            backticks = max((len(match.group(0)) for match in re.finditer(r"`+", command + output)), default=0)
            fence = "`" * max(3, backticks + 1)
            command_block = "%ssh\n%s\n%s" % (fence, command, fence)
            output_block = "%stext\n%s\n%s" % (fence, output, fence)
            verification_section = ("\n\n## Failing Verification\n\nCommand:\n%s\n\n- exit: %s\n\n"
                                    "Output tail:\n%s\n" %
                                    (command_block, call.get("verify_exit"), output_block))
    eligible_result = (linked_call and not call.get("guard")
                       and call.get("reason_code") != "worker_authority_violation"
                       and (successful_call or verified_failure))
    empty_salvage = False
    stopped_write = _stopped_write_continuation(last, eligible_result)
    if stopped_write:
        import panel_salvage
        recovery_evidence = panel_salvage._attempt_recovery_evidence(
            repo, task_id, last.get("attempt_id"), call, last, head_repo)
        if recovery_evidence:
            if all(item.startswith(".oms/salvage-provenance/") for item in recovery_evidence):
                message = ("recoverable delegate evidence exists for task %s and was already salvaged; "
                           "stopped write work is not transferred by --continue; inspect the recovery, then include "
                           "recovered work in a fresh dispatch brief: %s" %
                           (task_id, ", ".join(recovery_evidence)))
            else:
                message = ("recoverable delegate evidence exists for task %s; run oms panel --salvage %s "
                           "--attempt %s and inspect: %s" %
                           (task_id, task_id, last.get("attempt_id"), ", ".join(recovery_evidence)))
            panel_salvage._raise_recovery_evidence(recovery_evidence, message)
        payload_sha = panel_salvage._empty_provenance_identity(task_id, last.get("attempt_id"))
        relative = panel_salvage._empty_provenance_path(task_id, last.get("attempt_id"))
        try:
            provenance = panel_salvage._provenance(
                panel_salvage._read(Path(repo), relative, panel_salvage.MAX_EVIDENCE, True), payload_sha)
            empty_salvage = any(
                row.get("salvaged_from", {}).get("task") == task_id
                and row.get("salvaged_from", {}).get("attempt") == last.get("attempt_id")
                and row.get("salvaged_from", {}).get("outcome") == "nothing_to_recover"
                for row in provenance["records"])
        except (OSError, ValueError, TypeError):
            empty_salvage = False
        if not empty_salvage:
            raise ValueError("stopped write work is not transferred by --continue; first run oms panel --salvage %s "
                             "--attempt %s, inspect the recovery, then include recovered work in a fresh dispatch brief"
                             % (task_id, last["attempt_id"]))
    patch = call.get("patch") if eligible_result else None
    where = " The previous stopped attempt was recorded as having no patch to recover." if empty_salvage else ""
    drift_note = ""
    if patch:
        # Pass write continuations a verified artifact reference. Legacy callers
        # without a materialization channel retain the bounded inline context.
        try:
            body = verified_patch(repo, call)
        except (OSError, ValueError, TypeError):
            body = None
        name = os.path.relpath(Path(repo) / patch, repo)
        if body is None:
            if patch_reference is not None:
                raise ValueError("previous patch could not be verified; continuation refused")
            where = " The previous patch could not be verified and is not included."
        elif body and patch_reference is not None:
            import panel_results
            _, artifacts = panel_results._records(repo, _attempts=rows, task_id=task_id)
            record = next((row for row in reversed(artifacts)
                           if row.get("attempt_id") == last["attempt_id"] and row.get("kind") == "delegate"
                           and row.get("patch") == patch and row.get("patch_sha256") == call.get("patch_sha256")
                           and row.get("event_id") == call.get("event_id")), {})
            base = record.get("base_sha")
            if not re.fullmatch(r"[0-9a-f]{7,64}", str(base)):
                raise ValueError("previous patch has no recorded base; continuation refused")
            if _patch_in_head(head_repo, body):
                where = (" Your previous patch (%s) is already part of the repository at HEAD; continue from it."
                         % name)
            else:
                drift_note = _continuation_base_note(head_repo, base, body, allow_stale_base)
                patch_reference.update(patch=patch, sha256=call.get("patch_sha256"), base=base)
                where = (" The harness applies your previous patch (%s) at its original base before this round. "
                         "Continue from the applied changes; do not reapply it. The patch you return is against "
                         "that original base, not the current HEAD." % name)
                if drift_note:
                    where = " " + drift_note + "." + where
        elif body:
            excerpt = body.decode("utf-8", errors="replace")
            if len(excerpt.encode("utf-8")) + len(Path(brief).read_bytes()) + 8192 > MAX_PROMPT:
                raise ValueError("previous patch exceeds the continuation input budget; inspect and admit it in the "
                                 "owner checkout or include a bounded recovery in a fresh dispatch brief")
            where = " Your previous patch (%s) follows; apply what still fits.\n```diff\n%s```\n" % (name, excerpt)
        else:
            where = " The previous patch is empty; no patch is transferred."
    # A provider fallback swaps the attempt's provider after its row was written; the session belongs to
    # the provider that actually ran.
    ran = last.get("refs", {}).get("panel_provider") or last.get("provider")
    resume = session if (session and ran == route["provider"]
                         and session_stored(route["provider"], session)) else None
    if resume:
        if route["provider"] == "codex":
            usage = call.get("provider_usage")
            baseline = _codex_resume_usage_baseline(usage)
            if baseline:
                env["OMS_RESUME_USAGE_BASELINE"] = baseline
        preamble = ("Your earlier work was on %s; the repository is now at %s.%s%s\nNew instructions follow.\n\n"
                    % (old, head, where, verification_section))
        note = "continuing %s in its native session; %s" % (task_id, "verified patch carried forward" if patch and body else
                                                          "no patch transferred")
    else:
        has_summary = eligible_result and bool(call.get("answer"))
        summary = clean(str(call["answer"]), 1500) if has_summary else "no verified summary recorded"
        reason = ("the earlier worker ran on %s, not %s" % (ran, route["provider"])
                  if session and ran != route["provider"] else
                  "the earlier session is not recorded or no longer on disk")
        preamble = ("A fresh worker continues earlier work on %s (%s). The repository is now at %s.%s "
                    "Earlier summary: %s%s\nNew instructions follow.\n\n"
                    % (task_id, reason, head, where, summary, verification_section))
        note = "no resumable session (%s); fresh worker transfers %s and %s" % (
            reason, "verified patch" if patch and body else "no patch",
            "recorded summary" if has_summary else "no verified summary")
    if verification_section:
        note += "; verification failure attached"
    if drift_note:
        note += "; " + drift_note
    text = preamble + Path(brief).read_text(encoding="utf-8")
    if len(text.encode("utf-8")) > MAX_PROMPT:
        raise ValueError("brief plus continuation preamble exceeds the panel input contract")
    handle = tempfile.NamedTemporaryFile("w", suffix=".md", prefix="oms-continue-", encoding="utf-8", delete=False)
    with handle:
        handle.write(text)
    return Path(handle.name), resume, note
def moved_main(repo, owner, env):
    """(room, participant) of the Claude main this session continues, when its panel identity is gone.

    Claude Code's background service drops the panel environment but keeps
    CLAUDE_CODE_SESSION_ID, which the hook lineage rebind bound to the main."""
    session = env.get("CLAUDE_CODE_SESSION_ID")
    if (owner != "claude" or not session or env.get("OMS_PANEL_MAIN_ATTEMPT") or env.get("OMS_ROOM_PARTICIPANT")
            or env.get("OMS_HARNESS_CHILD") == "1" or env.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"):
        return None
    try:
        found = room.selected(repo, hashlib.sha256(session.encode()).hexdigest()[:32], env.get("OMS_ROOM_ID") or None)
        if not found or not found[0]:
            return None
        member = room.participant(room.status(repo, found[0]), found[1])
    except (OSError, ValueError):
        return None
    return found if member["role"] == "main" and member["provider"] == "claude" else None


def scope_overlaps(repo, room_id, state, caller, scopes):
    """Other mains', live write calls' and unresolved finished patches' scopes; coordination, not a lock."""
    from terminal_panel import _dispatch_call_finished
    review_state = state.get("scope_review_state", {})
    if not isinstance(review_state, dict):
        review_state = {}
    call_results = review_state.get("call_results", {})
    attempts = review_state.get("room_attempts", {})
    finalized = review_state.get("finalized")
    found = []
    for other in state["participants"]:
        if not other["joined"] or other["participant"] == caller or not other.get("owns"):
            continue
        if other.get("parent"):
            # Terminal attempts describe outcomes, not the lifetime of provider descendants.
            if _dispatch_call_finished(repo, room_id, other):
                result = call_results.get(other["participant"], {}) if isinstance(call_results, dict) else {}
                attempt = attempts.get(other["participant"], {}) if isinstance(attempts, dict) else {}
                task_id = result.get("task_id") if isinstance(result, dict) else None
                if (not task_id and isinstance(attempt, dict)):
                    task_id = attempt.get("task_id")
                pending_patch = (isinstance(finalized, dict) and isinstance(task_id, str) and task_id
                                 and task_id not in finalized and isinstance(result, dict)
                                 and result.get("task_id") == task_id and result.get("patch")
                                 and result.get("needs_review") is True and isinstance(attempt, dict)
                                 and attempt.get("task_id") == task_id and attempt.get("state") == "review")
                if pending_patch:
                    found += [{"participant": other["participant"], "role": other["role"],
                               "scope": theirs, "status": "finished, patch awaiting decision"}
                              for theirs in other["owns"]
                              if any(room.overlaps(mine, theirs) for mine in scopes)]
                continue
        elif other["role"] != "main":
            continue
        found += [{"participant": other["participant"], "role": other["role"], "scope": theirs}
                  for theirs in other["owns"] if any(room.overlaps(mine, theirs) for mine in scopes)]
    return found


def dispatch(repo, owner, role, workload, access, purpose, prompt=None, brief=None,
             verify=None, task_id=None, dry_run=False, label=None, target=None, model=None, effort=None,
             main=None, continue_task=None, scopes=None, work_repo=None, member_id=None, alias=None,
             candidate=None, allow_stale_base=False, from_task=None):
    from terminal_panel import WRITERS, _validate_dry_run_inputs, alternate_route, child_environment
    from terminal_panel import continuation, events, newest_route_model, provider_catalog, routing_status
    from terminal_panel import run_dispatch, safe_label, selected_route
    route = selected_route(owner, role, workload, access, purpose, target, model, effort, repo=repo, main=main,
                           task_id=task_id, candidate=candidate)
    if access == "write":
        _require_assigned_route(repo, task_id, route)
    if access == "write" and not verify and not scopes and not _plan_task_has_verifier(repo, task_id):
        print("WARN: write dispatch has neither a verify command nor a declared scope", file=sys.stderr)
    label = safe_label(label)
    alias_pattern = r"[A-Za-z0-9-]{1,8}"
    if alias is not None and not re.fullmatch(alias_pattern, alias):
        raise ValueError("--alias must be 1-8 letters, digits or hyphens")
    dispatch_env = child_environment(owner, repo)
    room_id = room.resolve(repo, dispatch_env.get("OMS_ROOM_ID"))
    call_rows = []
    if room_id:
        for attempt in json.loads(events(repo, "list", "--json", output=True)):
            refs = attempt.get("refs", {})
            if refs.get("panel_alias") and room.same_room(repo, refs.get("panel_room_id"), room_id):
                call_rows.append(attempt)
    if continue_task:
        inherited_writer_alias = False
        matches = {row.get("task_id") for row in call_rows
                   if row.get("refs", {}).get("panel_alias") == continue_task and row.get("task_id")}
        if matches:
            if len(matches) != 1:
                raise ValueError("call alias does not resolve to exactly one task in this room")
            continue_task = next(iter(matches))
        continued = next((row for row in reversed(call_rows)
                          if row.get("task_id") == continue_task
                          and row.get("refs", {}).get("panel_access") == "write"), None)
        if continued:
            existing_alias = continued.get("refs", {}).get("panel_alias")
            if alias is not None and existing_alias and alias != existing_alias:
                raise ValueError("--continue keeps the continued call's alias")
            alias = existing_alias or alias
            inherited_writer_alias = bool(existing_alias)
    resume = None
    resume_usage_baseline = None
    temporary = None
    temporaries = []
    continuation_patch = {} if continue_task and access == "write" else None
    if continue_task:
        if role not in WRITERS or not brief or prompt or task_id:
            raise ValueError("--continue takes a write-capable role and --brief-file, not --prompt or --task-id")
        if access == "write":
            _require_assigned_route(repo, continue_task, route)
        continuation_env = child_environment(owner, repo)
        continuation_env.pop("OMS_RESUME_USAGE_BASELINE", None)
        brief, resume, note = continuation(repo, owner, main, continue_task, route, brief,
                                           continuation_env, work_repo,
                                           patch_reference=continuation_patch,
                                           allow_stale_base=allow_stale_base)
        resume_usage_baseline = continuation_env.get("OMS_RESUME_USAGE_BASELINE")
        temporary, task_id = brief, continue_task
        print(note, file=sys.stderr)
    try:
        if temporary:
            temporaries.append(temporary)
            temporary = None
        # A dry run validates inputs only; prior findings join the brief that is actually sent.
        if (task_id or from_task) and brief and not dry_run:
            composed = _task_findings_brief(repo, [task_id] + list(from_task or []) if task_id else from_task, brief)
            if composed != brief:
                brief = composed
                temporaries.append(brief)
                _validate_dry_run_inputs(prompt, brief)
        fallback = None
        if not target:
            fallback = alternate_route(route, provider_status=routing_status(repo, None, main))
            if fallback:
                try:
                    peer = provider_catalog(fallback["provider"])[0]
                    if (peer["installed"] and access in peer["access"] and peer["exact_model_override"]):
                        fallback["binary"] = peer["binary"]
                        fallback["model"] = newest_route_model(
                            fallback["provider"], fallback["model"],
                            peer.get("catalog_models", peer.get("models", [])))
                    else:
                        fallback = None
                except (OSError, ValueError, KeyError, IndexError, subprocess.SubprocessError):
                    fallback = None
        options = {}
        if candidate is not None:
            options["candidate"] = candidate
        if continuation_patch is not None:
            options["continuation_patch"] = continuation_patch
        if continue_task:
            options["continue_call"] = inherited_writer_alias
        if resume_usage_baseline:
            options["resume_usage_baseline"] = resume_usage_baseline
        return run_dispatch(repo, owner, role, access, prompt, brief, verify, task_id,
                            dry_run, label, route, main, resume, scopes, work_repo, member_id, fallback, alias, **options)
    finally:
        for path in temporaries:
            path.unlink()


def _reserve_alias(repo, room_id, role, requested, call_rows):
    """Reserve an alias atomically until a lineage-resolved call row records it.

    Join aliases only count as used: a rollover does not carry finished members, so a join row
    cannot release a reservation without risking a second W1 in the successor room.
    """
    import panel_results
    alias_path = str(Path(repo) / ".oms/artifacts/panel-aliases.json")
    recorded = {row.get("refs", {}).get("panel_alias") for row in call_rows
                if row.get("refs", {}).get("panel_alias")}
    joined = {row.get("room_event", {}).get("alias") for row in room.records(repo, room_id)
              if row.get("room_event", {}).get("kind") == "join" and row.get("room_event", {}).get("alias")}
    try:
        with panel_results._lock(repo, "panel-aliases"):
            selected = []
            def mutate(old):
                try:
                    registry = json.loads(old) if old else {"schema": 1, "reservations": []}
                except (ValueError, TypeError):
                    raise ValueError("panel alias reservations are unreadable")
                if (not isinstance(registry, dict) or registry.get("schema") != 1
                        or not isinstance(registry.get("reservations"), list)):
                    raise ValueError("panel alias reservations are invalid")
                if any(not isinstance(row, dict) or not isinstance(row.get("room"), str)
                       or not isinstance(row.get("alias"), str) for row in registry["reservations"]):
                    raise ValueError("panel alias reservations are invalid")
                live_reservations = [row for row in registry["reservations"]
                                     if not (row["room"] == room_id or room.same_room(repo, row["room"], room_id))
                                     or row["alias"] not in recorded]
                registry["reservations"] = live_reservations
                reserved = {row["alias"] for row in live_reservations
                            if row["room"] == room_id or room.same_room(repo, row["room"], room_id)}
                used = reserved | recorded | joined
                value = requested
                if value is None:
                    letter = ROLE_ALIAS_LETTERS.get(role, "W")
                    numbers = [int(match.group(1)) for item in used if isinstance(item, str)
                               for match in [re.fullmatch(re.escape(letter) + r"(\d+)", item)] if match]
                    number = max(numbers or [0]) + 1
                    value = letter + str(number)
                elif value in used:
                    raise ValueError("call alias is already used in this room lineage")
                selected.append(value)
                registry["reservations"].append({"room": room_id, "alias": value})
                return (json.dumps(registry, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
            panel_results.DURABLE["atomic_mutate"](alias_path, mutate, str(repo), missing_ok=True,
                create_parent=True, label="panel alias reservations")
            return selected[0]
    except SystemExit as error:
        raise ValueError("panel alias reservation refused") from error


def _validate_dispatch_verifier(repo, verify):
    """Refuse missing suite files and unknown selectors before launching a worker."""
    from terminal_panel import _read_suite, _uses_smoke_runner
    if not isinstance(verify, str) or not verify.strip():
        return
    try:
        lexer = shlex.shlex(verify, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return
    separators = {"&&", "||", ";", "|", "&", ";;", "(", ")"}
    repo = Path(repo)
    for index, token in enumerate(tokens[:-1]):
        if token != "bash":
            continue
        suite = tokens[index + 1]
        if not re.fullmatch(r"(?:tests|scripts)/[A-Za-z0-9_.-]+\.sh", suite):
            continue
        target = repo / suite
        if not target.is_file():
            raise ValueError("--verify names missing suite file: %s" % suite)
        cursor = index + 2
        seen_sections = False
        while cursor < len(tokens):
            option = tokens[cursor]
            if option in separators:
                break  # selectors after a shell separator belong to the next command
            if option not in {"--section", "--only"}:
                cursor += 1
                continue
            if option == "--section" and suite == "tests/operator-tools-smoke.sh":
                if seen_sections:
                    raise ValueError("--verify repeats --section; use one comma-separated list")
                seen_sections = True
            if cursor + 1 >= len(tokens):
                break
            selector = tokens[cursor + 1]
            # Selectors are read from the suite text, never by running repository code (dry runs included).
            if option == "--section" and suite == "tests/operator-tools-smoke.sh":
                source = _read_suite(target)
                valid = [name.replace("_", "-") for name in
                         re.findall(r"^# OPERATOR_SECTION_BEGIN ([a-z_-]+)$", source, re.M)
                         if name not in {"core", "shared_cache"}]
                # Split-out sections are registered in the runner manifest.
                valid = list(dict.fromkeys(valid + re.findall(
                    r'^    \("([a-z/-]+)", [0-9]+, ', _read_suite(repo / "tests/operator/runner.py"), re.M)))
            elif option == "--only" and _uses_smoke_runner(suite, target):
                selected_suite = (repo / "tests/scripts-smoke.sh" if suite == "tests/run-smoke-shard.sh"
                                  else target)
                valid = re.findall(r"^(test_[A-Za-z0-9_]+)\(\) *\{", _read_suite(selected_suite), re.M)
            else:
                cursor += 2
                continue
            # run-smoke-shard.sh matches one --only argument by literal spaces; a tab, newline or
            # comma would select nothing and exit 0, so those stay part of an (unknown) name.
            if option == "--section":
                requested = selector.split(",")
            elif selector.startswith("test_"):
                requested = [name for name in selector.split(" ") if name]
            else:
                requested = [selector]
            unknown = [name for name in requested if name not in valid]
            if unknown:
                raise ValueError("unknown %s value(s): %s; valid names: %s" %
                                 (option, ", ".join(unknown), ", ".join(valid)))
            cursor += 2


def _plan_task_paths(repo, task_id):
    """Allowed and forbidden paths stored on the plan task; empty when absent."""
    if not task_id:
        return [], []
    from terminal_panel import _read
    try:
        raw = _read(Path(repo), ".oms/plan/tasks.json", 4 * 1024 * 1024, missing=True)
        tasks = json.loads(raw).get("tasks", {}) if raw else {}
        task = tasks.get(task_id) if isinstance(tasks, dict) else None
    except (OSError, ValueError, TypeError, AttributeError):
        return [], []
    if not isinstance(task, dict):
        return [], []
    def paths(key):
        value = task.get(key)
        return [item for item in value if isinstance(item, str) and item] if isinstance(value, list) else []
    return paths("allowed_paths"), paths("forbidden_paths")


def _plan_task_has_verifier(repo, task_id):
    """Check only the structured plan task contract; brief prose is not authority."""
    if not task_id:
        return False
    from terminal_panel import _read
    try:
        raw = _read(Path(repo), ".oms/plan/tasks.json", 4 * 1024 * 1024, missing=True)
        if not raw:
            return False
        document = json.loads(raw)
        tasks = document.get("tasks", {}) if isinstance(document, dict) else {}
        task = tasks.get(task_id) if isinstance(tasks, dict) else None
        return isinstance(task, dict) and isinstance(task.get("verify"), str) and bool(task["verify"].strip())
    except (OSError, ValueError, TypeError):
        return False


def _require_assigned_route(repo, task_id, route):
    """Refuse before any claim a route the plan task's reviewed assignment would reject at the worker."""
    if not task_id:
        return
    from terminal_panel import ROOT, _read
    import runpy
    try:
        raw = _read(Path(repo), ".oms/plan/tasks.json", 4 * 1024 * 1024, missing=True)
        tasks = json.loads(raw, object_pairs_hook=PlanBinding.unique_fields).get("tasks", {}) if raw else {}
        value = tasks.get(task_id, {}).get("assignment", {}) if isinstance(tasks, dict) else {}
    except (OSError, ValueError, TypeError, AttributeError):
        return
    assignment = runpy.run_path(str(ROOT / "scripts/lib/task-assignment.py"))["validate"](value)
    if not assignment:
        return
    effort = assignment.get("reasoning_effort")
    flags = "--to %s" % assignment["provider"] + "".join(
        " --%s %s" % (flag, assignment[key]) for flag, key in (("model", "model"), ("reasoning-effort", "reasoning_effort"))
        if assignment.get(key))
    if assignment.get("fallback_model") or effort not in (None, "low", "medium", "high"):
        raise ValueError("plan task %s has an assignment panel dispatch cannot express (fallback_model, or effort "
                         "other than low/medium/high); it names %s" % (task_id, flags))
    if (route["provider"] != assignment["provider"] or "model" in assignment and route["model"] != assignment["model"]
            or effort and route.get("effort") != effort):
        raise ValueError("plan task %s is reviewed for %s; dispatch with that route" % (task_id, flags))


def _read_suite(path):
    try:
        if path.is_symlink() or not path.is_file():
            raise ValueError("--verify suite must be a regular file: %s" % path.name)
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError("could not read --verify suite %s" % path.name) from error


def _uses_smoke_runner(suite, target):
    if suite == "tests/run-smoke-shard.sh":
        return True
    try:
        return "run-smoke-shard.sh" in target.read_text(encoding="utf-8")
    except OSError:
        return False


def plan_link(repo, owner, task_id, env, caller, access="read"):
    """Bind owned tasks for workers; leave ready read-only tasks informational."""
    from terminal_panel import ENTRY, PlanBinding, active_mains
    base = ["bash", str(ENTRY), "agent-plan", "--repo", str(repo)]
    plan_env = dict(env, OMS_ROOM_PARTICIPANT=caller) if caller else dict(env)
    for name in ("OMS_LEASE_ID", "OMS_PLAN_LEASE_ID"):
        plan_env.pop(name, None)

    def plan(*args):
        return subprocess.run(base + list(args), capture_output=True, text=True, check=False,
                              stdin=subprocess.DEVNULL, env=plan_env, timeout=60)

    def read_plan():
        from terminal_panel import PlanBinding, _read
        raw = _read(repo, ".oms/plan/tasks.json", 4 * 1024 * 1024, missing=True)
        if not raw and not (repo / ".oms/plan/tasks.json").exists():
            return {"tasks": {}}
        document = json.loads(raw, object_pairs_hook=PlanBinding.unique_fields)
        if not isinstance(document, dict) or not isinstance(document.get("tasks"), dict):
            raise ValueError("invalid plan task table")
        return document

    def snapshot():
        # The core reader is a compatibility reader; fence unsafe files before invoking it.
        from terminal_panel import PlanBinding
        read_plan()
        result = plan("evidence-snapshot", "--id", task_id)
        if result.returncode:
            # agent_plan_core.get_task/die exits 2; the oms front door appends this usage hint.
            if (result.returncode == 2 and result.stderr.splitlines() == ["error: no such task: " + task_id,
                        "hint: run `oms agent-plan --help` for usage"]
                    and not result.stdout.strip() and task_id not in read_plan()["tasks"]):
                return None
            raise ValueError(result.stderr.strip() or "plan snapshot unavailable")
        task = json.loads(result.stdout, object_pairs_hook=PlanBinding.unique_fields)
        if (not isinstance(task, dict) or task.get("id") != task_id
                or task.get("state") not in {"ready", "claimed", "running", "review", "landing", "blocked", "done", "cancelled"}):
            raise ValueError("invalid plan snapshot")
        return task

    def unprotected(reason):
        print("plan %s %s; plan protection is absent" % (task_id, reason), file=sys.stderr)

    task = snapshot()
    if task is None:
        unprotected("has no recorded task; panel left it unchanged")
        return None
    state, holder = task.get("state"), task.get("claimed_by_participant")
    if state == "cancelled":
        unprotected("is cancelled; panel left it unchanged")
        if access == "write":
            raise ValueError("plan task %s is cancelled; create a new task to continue" % task_id)
        return None
    if state == "ready" and access == "read":
        unprotected("is ready; panel left it unchanged")
        return None
    if state not in ("ready", "claimed", "running"):
        unprotected("is %s; panel left it unchanged" % state)
        if access == "write":
            raise ValueError(
                "plan task %s is not this main's to run; %s" % (task_id, (
                    "run `oms agent-plan reopen --id %s` first" % task_id if state == "blocked" else
                    "create a new task to continue" if state == "done" else
                    "finish or release it first")))
        return None
    if state != "ready" and holder != caller:
        unprotected("is %s by another claimant; panel left it unchanged" % state if holder else
                    "has an ownerless claim (another claimant); panel left it unchanged")
        if access == "write":
            raise ValueError(
                "plan task %s is not this main's to run; it is claimed by %s; inspect it with "
                "`oms agent-plan show --id %s` (or `recover-lease --check ...`), and release or "
                "recover it only if that claimant is gone" % (task_id, holder or "no owner", task_id))
        return None
    ident, attempt = env.get("OMS_ROOM_ID"), env.get("OMS_PANEL_MAIN_ATTEMPT")
    if not caller or not ident or not attempt:
        unprotected("has no owned main relationship; panel left it unchanged")
        if access == "write":
            raise ValueError("plan task %s is not this main's to run; dispatch it from the current room main"
                             % task_id)
        return None
    main = active_mains(repo, ident, room.project(room.records(repo, ident)), owner,
                        caller=caller, attempt=attempt).get(caller)
    if not main or main["attempt_id"] != attempt:
        raise ValueError("plan observation requires the current room main")
    if state == "ready":
        result = plan("claim", "--id", task_id, "--provider", owner)
        if result.returncode:
            raise ValueError(result.stderr.strip() or "claim failed")
        task = snapshot()
        if task is None:
            raise ValueError("plan task disappeared after claim")
        state, holder = task.get("state"), task.get("claimed_by_participant")
    if state not in ("claimed", "running") or holder != caller:
        raise ValueError("plan claim changed while selecting observation")
    binding = PlanBinding.parse(json.dumps(dict(plan_id=task.get("plan_id"), task_id=task_id,
        lease_id=task.get("lease_id"), claim_provider=task.get("provider"), owner_provider=owner,
        owner_participant=caller, owner_attempt=attempt, room_id=ident)))
    if task.get("claimed_by_participant") != caller:
        raise ValueError("plan claimant changed while selecting observation")
    if task.get("state") == "claimed":
        result = plan("start", "--id", task_id, "--lease-id", binding.lease_id)
        if result.returncode:
            raise ValueError(result.stderr.strip() or "start failed")
    if access == "read":
        print("plan %s linked to a read-only call; record the owner decision with oms panel --finalize"
              % task_id, file=sys.stderr)
    else:
        print("plan %s running (lease %s): after acceptance oms agent-plan review --id %s --lease-id %s "
              "--artifact PATH [--patch PATH]; after oms land: oms agent-plan finish --id %s --landed-commit SHA"
              % (task_id, binding.lease_id, task_id, binding.lease_id, task_id), file=sys.stderr)
    return binding


def _read_dispatch_brief(path):
    """Read a bounded regular brief without following a symlink."""
    from terminal_panel import MAX_PROMPT
    try:
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise ValueError("brief must be a readable regular file of at most 64 KiB")
        if info.st_size > MAX_PROMPT or not info.st_mode & 0o444:
            raise ValueError("brief must be a readable regular file of at most 64 KiB")
        with path.open("rb") as handle:
            opened = os.fstat(handle.fileno())
            if (not stat.S_ISREG(opened.st_mode) or
                    (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino)):
                raise ValueError("brief changed while being read")
            content = handle.read(MAX_PROMPT + 1)
    except OSError as error:
        raise ValueError("brief must be a readable regular file of at most 64 KiB") from error
    if len(content) > MAX_PROMPT:
        raise ValueError("brief must be a readable regular file of at most 64 KiB")
    return content


def _validate_dry_run_inputs(prompt, brief):
    """Use the shared outbound scanner before returning any dispatch plan."""
    from terminal_panel import MAX_PROMPT, OUTBOUND_SCANNER, _read_dispatch_brief
    if prompt and (len(prompt.encode("utf-8")) > MAX_PROMPT or "\0" in prompt):
        raise ValueError("prompt exceeds the panel input contract")
    inputs = []
    if prompt:
        inputs.append(prompt.encode("utf-8"))
    if brief:
        inputs.append(_read_dispatch_brief(Path(brief)))
    if not inputs:
        return
    with tempfile.TemporaryDirectory(prefix="oms-panel-scan-") as temporary:
        for index, content in enumerate(inputs):
            candidate = Path(temporary) / ("input-%d" % index)
            candidate.write_bytes(content)
            checked = subprocess.run(
                ["bash", "-c", '. "$1"; ma_validate_outbound_prompt "$2"',
                 "oms-outbound-scan", str(OUTBOUND_SCANNER), str(candidate)],
                stdout=subprocess.DEVNULL, stderr=None, check=False)
            if checked.returncode:
                if checked.returncode == 3:
                    raise ValueError("outbound dispatch input contains sensitive-looking content")
                raise ValueError("outbound dispatch input could not be validated")


def _task_findings_brief(repo, task_id, brief):
    import panel_results
    try:
        original = _read_dispatch_brief(Path(brief)).decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        # The dispatch path reports an unreadable brief itself; findings never change that error.
        return brief
    section = panel_results.prior_findings(repo, task_id, original)
    if not section:
        return brief
    handle = tempfile.NamedTemporaryFile("w", suffix=".md", prefix="oms-task-findings-",
                                         encoding="utf-8", delete=False)
    with handle:
        handle.write(original + section)
    return Path(handle.name)


def _rollover_if_needed(repo, room_id, state, caller):
    """Continue a room past its rollover marks as its successor; the capacity check refuses only if this fails."""
    from terminal_panel import _adopt_successor, clean
    if (len(state["messages"]) <= room.ROLLOVER_MESSAGES
            and sum(bool(p.get("parent")) for p in state["participants"]) <= room.ROLLOVER_CALLS):
        return None
    try:
        moved = room.rollover(repo, room_id, caller)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print("Room %s could not roll over: %s" % (room_id, clean(str(error), 200)), file=sys.stderr)
        # Past its seal the room has moved on; its successor refuses writes until the next one finishes the carry.
        moved = room.resolve(repo, room_id, fresh=True)
        if moved == room_id:
            return None
    print("Room %s continues as %s; ids naming %s keep working." % (room_id, moved, room_id), file=sys.stderr)
    _adopt_successor(repo)
    return moved


def _adopt_successor(repo, session=None):
    """Point this panel's tmux room bindings at the room they now continue as; room logs are not written."""
    from terminal_panel import managed_session, recorded_room, tmux_command
    session = session or (os.environ.get("OMS_PANEL_SESSION", "") if managed_session() else "")
    if not session or not shutil.which("tmux"):
        return
    try:
        recorded = recorded_room(session)
        if recorded and room.resolve(repo, recorded) != recorded:
            subprocess.run(tmux_command("set-environment", "-t", "=" + session, "OMS_ROOM_ID",
                                        room.resolve(repo, recorded)),
                           capture_output=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        found = subprocess.run(tmux_command("list-windows", "-t", "=" + session, "-F",
                                            "#{window_id}\t#{@oms_panel_room}"),
                               capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        for line in found.stdout.splitlines()[:128] if found.returncode == 0 else []:
            window, _, value = line.replace("\r", "").partition("\t")
            if re.fullmatch(r"@[0-9]+", window) and value and room.resolve(repo, value) != value:
                subprocess.run(tmux_command("set-option", "-w", "-t", window, "@oms_panel_room",
                                            room.resolve(repo, value)),
                               capture_output=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
    except (OSError, ValueError, subprocess.SubprocessError):
        pass


def _dispatch_capacity(room_id, state):
    # A main summons as many calls as its work needs; only the room's own bounds apply.
    if (len(state["messages"]) + 2 > room.MAX_MESSAGES
            or sum(bool(p.get("parent")) for p in state["participants"]) >= room.MAX_CALLS):
        raise ValueError("room %s is full" % room_id)
    if state["closed"]:
        raise ValueError("room is closed")


def _dispatch_group_gone(pid):
    if type(pid) is not int or pid <= 0:
        return False
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return True
    except OSError:
        pass
    return False


def _dispatch_exited(process, identity, reaped=False):
    from terminal_panel import _dispatch_group_gone, _dispatch_leader_gone, _dispatch_session_gone
    reaped = reaped or type(process.poll()) is int
    if os.name != "posix":
        # Windows retains its reaped-child check; session teardown cannot be attested here.
        return reaped
    leader_gone = reaped or _dispatch_leader_gone(process.pid, identity)
    return bool(leader_gone and _dispatch_group_gone(process.pid) and _dispatch_session_gone(process.pid))


def _dispatch_leader_gone(pid, identity):
    if type(pid) is not int or pid <= 0:
        return False
    if identity:
        import panel_terminals
        return panel_terminals.process_gone(pid, identity)
    # A quick leader may disappear before its start identity can be recorded.
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except OSError:
        pass
    return False


def _dispatch_process_snapshot():
    """Read Linux topology and start identity together before short-lived shell helpers disappear."""
    boot = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="utf-8", errors="replace").strip()
    if not boot:
        raise ValueError("process boot identity unavailable")
    rows, deadline = {}, time.monotonic() + 3
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        if time.monotonic() >= deadline:
            raise ValueError("process observation timed out")
        child = int(entry.name)
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
        except FileNotFoundError:
            try:
                os.kill(child, 0)
            except ProcessLookupError:
                continue
            raise ValueError("process observation unavailable")
        if not fields[19].isdigit():
            raise ValueError("process start identity unavailable")
        rows[child] = {"pid": child, "parent": int(fields[1]), "group": int(fields[2]),
                       "session": int(fields[3]),
                       "identity": hashlib.sha256((boot + ":" + fields[19]).encode("utf-8")).hexdigest()}
    return rows


def _dispatch_session_gone(pid):
    """A reaped leader's session can outlive both it and its original group."""
    from terminal_panel import _dispatch_process_snapshot
    if type(pid) is not int or pid <= 0:
        return False
    try:
        if sys.platform == "linux":
            rows = _dispatch_process_snapshot()
            return bool(rows) and not any(row["session"] == pid for row in rows.values())
        observed = subprocess.run(["ps", "-e", "-o", "pid="], capture_output=True,
                                  text=True, timeout=3, check=False)
        if observed.returncode or not observed.stdout.strip():
            return False
        for value in observed.stdout.replace("\r", "").split():
            child = int(value)
            if child <= 0:
                return False
            try:
                if os.getsid(child) == pid:
                    return False
            except ProcessLookupError:
                continue
        return True
    except (OSError, ValueError, IndexError, KeyError, subprocess.SubprocessError):
        return False


def _dispatch_descendants(pid):
    """Snapshot provider descendants and session members; incomplete observation cannot authorize settlement."""
    from terminal_panel import _dispatch_process_snapshot
    if os.name != "posix":
        return None
    import panel_terminals
    try:
        snapshot = _dispatch_process_snapshot() if sys.platform == "linux" else None
        if snapshot is not None:
            parents = {child: row["parent"] for child, row in snapshot.items()}
        else:
            observed = subprocess.run(["ps", "-e", "-o", "pid=,ppid="], capture_output=True,
                                      text=True, timeout=3, check=False)
            if observed.returncode:
                return None
            parents = dict(tuple(map(int, line.split())) for line in observed.stdout.replace("\r", "").splitlines())
        subtree, remaining = {pid}, set(parents) - {pid}
        # Session members may have been reparented after an intermediate shell exits.
        for child in remaining:
            try:
                if (snapshot[child]["session"] if snapshot is not None else os.getsid(child)) == pid:
                    subtree.add(child)
            except ProcessLookupError:
                continue
        remaining.difference_update(subtree)
        if len(subtree) > 256:
            return None
        while True:
            children = {child for child in remaining if parents[child] in subtree}
            if not children:
                break
            subtree.update(children)
            remaining.difference_update(children)
            if len(subtree) > 256:
                return None
        records = []
        for child in sorted(subtree - {pid}):
            current = snapshot[child] if snapshot is not None else panel_terminals.process(child)
            if not current or current["parent"] != parents[child]:
                return None
            records.append({key: current[key] for key in ("pid", "identity", "group")})
        return records
    except (OSError, ValueError, IndexError, KeyError, subprocess.SubprocessError):
        return None


def _observe_dispatch_descendants(process, evidence):
    from terminal_panel import _dispatch_descendants
    current = _dispatch_descendants(process.pid)
    if current is None:
        evidence["observation_failed"] = True
        evidence.setdefault("descendants", None)
        return
    if "descendants" not in evidence:
        evidence["descendants"] = current
    else:
        # Retain known children for teardown, without treating a gap as complete evidence.
        known = {(row["pid"], row["identity"]): row for row in (evidence["descendants"] or []) + current}
        evidence["descendants"] = list(known.values()) if len(known) <= 256 else None
        if len(known) > 256:
            evidence["observation_failed"] = True


def _dispatch_descendants_gone(records):
    from terminal_panel import _dispatch_group_gone
    if not isinstance(records, list) or len(records) > 256:
        return False
    import panel_terminals
    for row in records:
        if (not isinstance(row, dict) or set(row) != {"pid", "identity", "group"}
                or type(row["pid"]) is not int or row["pid"] <= 0
                or not isinstance(row["identity"], str) or not row["identity"]
                or type(row["group"]) is not int or row["group"] <= 0
                or not panel_terminals.process_gone(row["pid"], row["identity"])
                or not _dispatch_group_gone(row["group"])):
            return False
    return True


def _dispatch_recorded_gone(records):
    from terminal_panel import _dispatch_descendants_gone
    return _dispatch_descendants_gone(records)


def _dispatch_call_path(member, exited=False):
    if not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", member or ""):
        raise ValueError("invalid dispatch call identity")
    return ".oms/artifacts/panel-" + member + ("-exit" if exited else "") + ".json"


def _dispatch_pending_path(member):
    from terminal_panel import _dispatch_call_path
    return _dispatch_call_path(member)[:-5] + "-pending.json"


def _dispatch_exit_record(call, status):
    launch = json.dumps(call, ensure_ascii=False, sort_keys=True, allow_nan=False).encode("utf-8")
    return dict(call, exit=status, launch_sha256=hashlib.sha256(launch).hexdigest(),
                output_log=".oms/panel-state/dispatch-logs/%s.log" % call["participant"])


def _dispatch_output_log(repo, member):
    """Create a private, bounded transcript for one dispatched call."""
    relative = ".oms/panel-state/dispatch-logs/%s.log" % member
    directory = Path(repo) / ".oms/panel-state/dispatch-logs"
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.lstat()
    if (not stat.S_ISDIR(info.st_mode) or os.name == "posix" and
            (info.st_mode & 0o077 or hasattr(os, "getuid") and info.st_uid != os.getuid())):
        raise ValueError("unsafe dispatch output directory")
    fd = os.open(str(directory / (member + ".log")),
                 os.O_RDWR | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    return relative, fd


def _tee_dispatch_output(stream, destination, tail, lock):
    """Forward one child stream unchanged while adding it to the shared bounded log tail."""
    if not isinstance(stream, io.IOBase):
        return
    try:
        while True:
            # read1 returns what is available, so output streams live instead of waiting for 8 KiB or EOF.
            chunk = stream.read1(8192) if hasattr(stream, "read1") else stream.read(8192)
            if not chunk:
                break
            with lock:
                tail.extend(chunk)
                if len(tail) > 65536:
                    del tail[:-65536]
            try:
                destination.buffer.write(chunk)
                destination.buffer.flush()
            except (AttributeError, OSError):
                destination.write(chunk.decode("utf-8", "replace"))
                destination.flush()
    finally:
        stream.close()


def _save_dispatch_output(fd, tail):
    with os.fdopen(fd, "wb") as log:
        log.write(tail)
        log.flush()
        os.fsync(log.fileno())


def _dispatch_verifier(repo, room_id, member, caller, task_id=None):
    from terminal_panel import _dispatch_call_record
    call = _dispatch_call_record(repo, room_id, {"participant": member, "parent": caller})
    if (not call or call.get("schema") != 2
            or task_id is not None and call.get("task_id") != task_id):
        return None
    anchor = dispatch_store.read(repo, member + "-launch.json")
    verify = anchor.get("verify") if isinstance(anchor, dict) else None
    return verify if isinstance(verify, str) and verify.strip() else None


def _prepare_dispatch_call(repo, room_id, member, caller, verify=None, execution_repo=None, rework=None,
                           task_id=None):
    from terminal_panel import DURABLE, _dispatch_call_path, _write_brief_artifact
    call = {"schema": 2, "kind": "oms-panel-call", "room": room_id, "participant": member,
            "parent": caller, "nonce": uuid.uuid4().hex,
            "backend": "subreaper" if sys.platform == "linux" else "unsupported"}
    if task_id is not None:
        call["task_id"] = task_id
    dispatch_store.initialize(repo, member, call, execution_repo)
    path = Path(repo) / _dispatch_call_path(member)
    # The existing durable writer anchors parents, refuses links and creates 0600.
    # Even an empty precreated file must refuse launch, rather than become evidence.
    try:
        DURABLE["atomic_mutate"](str(path), lambda old: dispatch_store.encoded(call), str(repo),
                                 missing_ok=True, create_parent=True, expected_exists=False,
                                 label="dispatch launch")
    except SystemExit as error:
        raise ValueError("dispatch launch record refused") from error
    anchor = {"call": call, "generation": dispatch_store.generation(path)}
    if verify:
        anchor["verify"] = verify
    if execution_repo:
        anchor["execution_repo"] = str(execution_repo)
    if rework:
        original = dict(rework)
        content = original.pop("brief")
        relative = _dispatch_call_path(member)[:-5] + "-brief.md"
        _write_brief_artifact(repo, relative, content)
        anchor["rework"] = dict(original, brief=relative, brief_sha256=hashlib.sha256(content.encode("utf-8")).hexdigest())
    dispatch_store.write(repo, member + "-launch.json", anchor, exclusive=True)
    return call


def _valid_rework_context(context, member):
    from terminal_panel import TIERS, WRITERS, _dispatch_call_path
    return (isinstance(context, dict) and set(context) == {"role", "scope", "workload", "brief", "brief_sha256"}
            and context.get("role") in WRITERS and context.get("workload") in TIERS
            and isinstance(context.get("scope"), list)
            and all(isinstance(path, str) and room.normalize_scope(path) == path for path in context["scope"])
            and context.get("brief") == _dispatch_call_path(member)[:-5] + "-brief.md"
            and isinstance(context.get("brief_sha256"), str)
            and re.fullmatch(r"[0-9a-f]{64}", context["brief_sha256"]))


def _dispatch_call_record(repo, room_id, member):
    from terminal_panel import _dispatch_call_path, _read, _valid_rework_context
    try:
        name = member["participant"]
        path = Path(repo) / _dispatch_call_path(name)
        anchor = dispatch_store.read(repo, name + "-launch.json")
        data = _read(repo, _dispatch_call_path(name), 4096)
        call = anchor["call"]
        if (not isinstance(call, dict) or type(call.get("schema")) is not int
                or call["schema"] not in {1, 2}
                or anchor["generation"] != dispatch_store.generation(path)
                or call["kind"] != "oms-panel-call"
                or not room.same_room(repo, call["room"], room_id) or call["participant"] != name
                or call["parent"] != member.get("parent")):
            return None
        fields = {"schema", "kind", "room", "participant", "parent"}
        if call["schema"] == 2:
            # Accept older control_root diagnostics without using them for discovery.
            required = fields | {"nonce", "backend"}
            if (not required <= set(call) or set(call) - required - {"task_id", "control_root"}
                    or data != dispatch_store.encoded(call)
                    or not isinstance(call["nonce"], str) or not re.fullmatch(r"[0-9a-f]{32}", call["nonce"])
                    or call["backend"] not in {"subreaper", "unsupported"}
                    or "control_root" in call and (not isinstance(call["control_root"], str)
                                                    or not os.path.isabs(call["control_root"]))
                    or "task_id" in call and (not isinstance(call["task_id"], str)
                                                or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", call["task_id"]))
                    or set(anchor) - {"call", "generation", "verify", "execution_repo", "rework"}
                    or "rework" in anchor and not _valid_rework_context(anchor["rework"], name)
                    or "verify" in anchor and (not isinstance(anchor["verify"], str) or not anchor["verify"].strip())
                    or "execution_repo" in anchor and (not isinstance(anchor["execution_repo"], str)
                                                         or not os.path.isabs(anchor["execution_repo"]))):
                return None
        else:
            # Old repository records alone cannot establish controller provenance.
            # Retain their writer's newline only with the same private launch anchor.
            if (set(call) != fields | {"pid", "identity", "session"}
                    or data != dispatch_store.encoded(call) + b"\n"
                    or type(call["pid"]) is not int or call["pid"] <= 0
                    or type(call["session"]) is not bool or not call["session"] or os.name != "posix"
                    or not isinstance(call["identity"], str) or not call["identity"]):
                return None
        return call
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return None


def _record_dispatch_pending(repo, call, status, task_id, evidence):
    from terminal_panel import _dispatch_call_record, _dispatch_exit_record, _dispatch_pending_path, _write
    current = _dispatch_call_record(repo, call["room"], call)
    if current != call:
        raise ValueError("dispatch launch changed after launch")
    pending = dict(call, exit=status, task_id=task_id, forced=bool(evidence.get("forced")),
                   descendants=None if evidence.get("observation_failed") else evidence.get("descendants"), launch_sha256=_dispatch_exit_record(call, status)["launch_sha256"])
    dispatch_store.write(repo, call["participant"] + "-pending.json", pending)
    # The immutable repository copy is diagnostic; only the private current record is authority.
    try:
        _write(repo, _dispatch_pending_path(call["participant"]), pending)
    except (OSError, ValueError):
        pass


class DispatchEvidenceError(ValueError):
    def __init__(self, participant, record, reason):
        self.participant = participant
        self.record = record
        self.reason = reason
        super().__init__("%s for %s: %s" % (record, participant, reason))


def _dispatch_pending(repo, call):
    from terminal_panel import _dispatch_exit_record
    try:
        pending = dispatch_store.read(repo, call["participant"] + "-pending.json")
    except FileNotFoundError:
        # A launch anchored in a prior boot is sufficient to settle an attempt
        # interrupted before its controller could publish pending evidence.
        if not dispatch_store.stale_boot(repo, call["participant"] + "-launch.json"):
            raise
        status = _dispatch_stale_exit(repo, call, None)
        if status is None:
            status = 143
        task_id = call.get("task_id") or call["participant"]
        return dict(call, exit=status, task_id=task_id, forced=False,
                    descendants=None, launch_sha256=_dispatch_exit_record(call, status)["launch_sha256"],
                    recovered=True)
    if (not isinstance(pending, dict) or set(pending) != set(call) | {"exit", "task_id", "forced", "descendants", "launch_sha256"}
            or any(pending[key] != value for key, value in call.items())
            or type(pending["exit"]) is not int or type(pending["forced"]) is not bool
            or pending["launch_sha256"] != _dispatch_exit_record(call, pending["exit"])["launch_sha256"]
            or not isinstance(pending["task_id"], str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", pending["task_id"])):
        raise ValueError("invalid pending call record")
    stale = any(_dispatch_stale_boot(repo, call, suffix)
                for suffix in ("-launch.json", "-process.json", "-pending.json", "-supervisor.json"))
    if stale:
        exit_status = _dispatch_stale_exit(repo, call, pending)
        if exit_status is None:
            exit_status = 143
        pending = dict(pending, exit=exit_status,
                       launch_sha256=_dispatch_exit_record(call, exit_status)["launch_sha256"])
    return pending


def _dispatch_stale_boot(repo, call, suffix):
    try:
        return dispatch_store.stale_boot(repo, call["participant"] + suffix)
    except FileNotFoundError:
        return False
    except (OSError, ValueError, TypeError):
        record = {"-supervisor.json": "supervisor receipt", "-process.json": "process binding",
                  "-launch.json": "launch receipt", "-pending.json": "pending record"}.get(suffix, "dispatch record")
        raise DispatchEvidenceError(call["participant"], record, "malformed or foreign record")


def _dispatch_supervisor_snapshot(repo, call, allow_stale=False):
    from terminal_panel import _dispatch_exit_record
    if call["schema"] == 2 and call["backend"] != "subreaper":
        return None
    if (not allow_stale and (_dispatch_stale_boot(repo, call, "-process.json")
                             or _dispatch_stale_boot(repo, call, "-supervisor.json"))):
        return None
    errors = []
    try:
        receipt = dispatch_store.read(repo, call["participant"] + "-supervisor.json")
    except FileNotFoundError:
        receipt = None
    except (OSError, ValueError, TypeError):
        receipt = None
        errors.append(DispatchEvidenceError(call["participant"], "supervisor receipt",
                                            "malformed or foreign record"))
    try:
        binding = dispatch_store.read(repo, call["participant"] + "-process.json")
    except FileNotFoundError:
        binding = None
    except (OSError, ValueError, TypeError):
        binding = None
        errors.append(DispatchEvidenceError(call["participant"], "process binding",
                                            "malformed or foreign record"))
    if errors:
        raise errors[0]
    if (receipt is not None and
            (not isinstance(receipt, dict) or set(receipt) != {"launch_sha256", "pid", "all_reaped", "exit"}
             or type(receipt.get("pid")) is not int or receipt["pid"] <= 0
             or type(receipt.get("all_reaped")) is not bool
             or receipt.get("exit") is not None and type(receipt["exit"]) is not int)):
        raise DispatchEvidenceError(call["participant"], "supervisor receipt", "malformed record")
    digest = _dispatch_exit_record(call, 0)["launch_sha256"]
    if receipt is not None and receipt["launch_sha256"] != digest:
        raise DispatchEvidenceError(call["participant"], "supervisor receipt", "bound to another launch")
    if (binding is not None and
            (not isinstance(binding, dict) or set(binding) not in ({"pid"}, {"pid", "identity", "group"})
             or type(binding.get("pid")) is not int or binding["pid"] <= 0
             or set(binding) == {"pid", "identity", "group"}
             and (not isinstance(binding["identity"], str) or not binding["identity"]
                  or type(binding["group"]) is not int or binding["group"] != binding["pid"]))):
        raise DispatchEvidenceError(call["participant"], "process binding", "malformed record")
    if receipt is None or binding is None:
        return None
    if (receipt["pid"] != binding["pid"]
            or call["schema"] == 1 and binding["pid"] != call["pid"]):
        raise DispatchEvidenceError(call["participant"], "process binding", "does not match the launch receipt")
    return receipt, binding


def _dispatch_stale_exit(repo, call, pending):
    snapshot = _dispatch_supervisor_snapshot(repo, call, allow_stale=True)
    if snapshot:
        receipt, unused_binding = snapshot
        if type(receipt["exit"]) is int:
            return receipt["exit"]
    # run_dispatch starts with status 1; a zero pending status can only be
    # recorded after it observed the child exit, even without a reaped receipt.
    if pending and pending["exit"] == 0:
        return 0
    return None


def _dispatch_supervisor_receipt(repo, call):
    from terminal_panel import _dispatch_supervisor_snapshot
    snapshot = _dispatch_supervisor_snapshot(repo, call)
    return snapshot[0] if snapshot else None


def _cancel_watch_authority(repo, main_attempt, owner):
    """Require this window's own live main watch, never an observational/child caller."""
    from terminal_panel import managed_session, pane_local_option, pane_option
    env = os.environ
    if (env.get("OMS_HARNESS_CHILD") == "1" or env.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"
            or env.get("OMS_PANEL_DISPATCH") or env.get("OMS_PANEL_ROLE")
            or env.get("OMS_ATTEMPT_ID") or env.get("OMS_TASK_ID")
            or env.get("OMS_ROOM_ADMITTED_PARTICIPANT")
            or env.get("OMS_ROOM_PARTICIPANT") not in (None, owner)):
        return False
    pane = env.get("TMUX_PANE", "")
    if not managed_session() or not re.fullmatch(r"%[0-9]+", pane):
        return False
    try:
        repo_path = str(Path(repo).resolve())
        options = {name: pane_option(name) for name in
                   ("@oms_panel_main_attempt", "@oms_panel_room_participant", "@oms_panel_repo",
                    "@oms_panel_native_pane")}
        return (options["@oms_panel_main_attempt"] == main_attempt
                and options["@oms_panel_room_participant"] == owner
                and options["@oms_panel_repo"] == repo_path
                and pane_local_option("@oms_panel_main_watch") == "1"
                and re.fullmatch(r"%[0-9]+", options["@oms_panel_native_pane"]) is not None
                and options["@oms_panel_native_pane"] != pane)
    except (OSError, ValueError, TypeError, subprocess.SubprocessError):
        return False


def cancel_selected_call(repo, state, navigation, main_attempt, send=False):
    """Signal one still-live child supervisor after checking panel, ledger and PID identity."""
    from terminal_panel import _cancel_watch_authority, _dispatch_call_record, _dispatch_supervisor_snapshot
    from terminal_panel import events
    selected = navigation.get("selected") or ()
    owner_binding = navigation.get("cancel_owner") or {}
    room_id = state.get("room", {}).get("id")
    if (len(selected) != 2 or selected[0] != "result"
            or not isinstance(owner_binding, dict)):
        return "Select a running call owned by this main"
    owner = owner_binding.get("participant")
    watched_attempt = owner_binding.get("attempt")
    if not isinstance(owner, str) or not owner or watched_attempt != main_attempt:
        return "Selected main does not match this panel"
    if not isinstance(main_attempt, str) or not main_attempt:
        return "Panel main identity is unavailable"
    if not _cancel_watch_authority(repo, main_attempt, owner):
        return "Only this panel's own main watch can cancel calls"
    try:
        room_state = room.status(repo, room_id) if room_id else None
        members = (room_state or {}).get("participants") or []
        main = next((row for row in members if row.get("participant") == owner), None)
        member = next((row for row in members if row.get("participant") == selected[1]), None)
        if (not main or not main.get("joined") or main.get("role") != "main"
                or not member or not member.get("joined") or member.get("role") == "main"):
            return "Selected call is no longer available"
        if member.get("parent") != owner:
            return "Cannot cancel a call owned by another main"
        main_row = json.loads(events(repo, "show", "--attempt", main_attempt, "--json", output=True))
        main_refs = main_row.get("refs") or {}
        if (main_row.get("tool") != "panel-main" or main_row.get("terminal")
                or not room.same_room(repo, main_refs.get("panel_room_id", room_id), room_id)
                or main_refs.get("panel_room_participant", main_attempt) != owner):
            return "Panel main is no longer live"
        attempt = (state.get("room_attempts") or {}).get(member["participant"])
        if not isinstance(attempt, dict) or attempt.get("state") not in {"starting", "working", "verifying"}:
            return "Selected call is no longer running"
        current = json.loads(events(repo, "show", "--attempt", attempt.get("attempt_id"), "--json", output=True))
        refs = current.get("refs") or {}
        if (current.get("attempt_id") != attempt.get("attempt_id") or current.get("terminal")
                or current.get("state") not in {"starting", "working", "verifying"}
                or current.get("parent_attempt_id") != main_attempt
                or not room.same_room(repo, refs.get("panel_room_id"), room_id)
                or refs.get("panel_room_participant") != member["participant"]):
            return "Selected call is no longer running"
        call = _dispatch_call_record(repo, room_id, member)
        if not call or call.get("parent") != owner or call.get("backend") != "subreaper":
            return "Selected call has no live supervisor record"
        snapshot = _dispatch_supervisor_snapshot(repo, call)
        if not snapshot:
            return "Supervisor identity is unavailable; call was not signalled"
        receipt, binding = snapshot
        if receipt["all_reaped"] or receipt["exit"] is not None:
            return "Selected call has already settled"
        if set(binding) != {"pid", "identity", "group"}:
            return "Legacy supervisor binding cannot be safely cancelled; call was not signalled"
        if not send:
            return "ready"
        import panel_terminals
        if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            return "Safe supervisor signalling is unavailable; call was not signalled"
        pidfd = os.pidfd_open(binding["pid"])
        try:
            current_process = panel_terminals.process(binding["pid"])
            if (not current_process or current_process["identity"] != binding["identity"]
                    or current_process["group"] != binding["group"]
                    or os.getpgid(binding["pid"]) != binding["group"]):
                return "Supervisor identity changed; call was not signalled"
            # A pidfd pins the verified process across the signal and prevents PID reuse.
            signal.pidfd_send_signal(pidfd, signal.SIGTERM)
        finally:
            os.close(pidfd)
        return "Cancellation requested; waiting for supervisor cleanup"
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        return "Call cancellation could not be verified; call was not signalled"


def _dispatch_call_finished(repo, room_id, member, report_evidence=False):
    from terminal_panel import _dispatch_call_record, _dispatch_group_gone, _dispatch_pending
    from terminal_panel import _dispatch_session_gone, _dispatch_supervisor_receipt
    call = _dispatch_call_record(repo, room_id, member)
    if not call:
        return False
    try:
        _dispatch_pending(repo, call)
        if any(_dispatch_stale_boot(repo, call, suffix)
               for suffix in ("-launch.json", "-process.json", "-pending.json", "-supervisor.json")):
            return True
        receipt = _dispatch_supervisor_receipt(repo, call)
        if not (receipt and receipt["all_reaped"] and type(receipt["exit"]) is int):
            return False
        if call["schema"] == 1:
            import panel_terminals
            return (panel_terminals.process_gone(call["pid"], call["identity"])
                    and _dispatch_group_gone(call["pid"]) and _dispatch_session_gone(call["pid"]))
        return True
    except DispatchEvidenceError:
        if report_evidence:
            raise
        return False
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return False


def _dispatch_released(repo, call):
    from terminal_panel import _dispatch_exit_record
    try:
        released = dispatch_store.read(repo, call["participant"] + "-settled.json")
        return (released.get("released") is True and type(released.get("exit")) is int
                and released.get("launch_sha256") == _dispatch_exit_record(call, 0)["launch_sha256"])
    except (OSError, ValueError, KeyError, TypeError):
        return False


def _reconcile_dispatch(repo, room_id, state, caller, parent):
    """Only dispatch actions may settle deferred calls; board reads stay observational."""
    from terminal_panel import _dispatch_call_finished, _dispatch_call_record, _dispatch_pending
    from terminal_panel import _dispatch_released, _settlement_delivery_pending, deliver_result, events
    rows = json.loads(events(repo, "list", "--json", output=True))
    import shlex
    for orphan in dispatch_store.orphan_members(repo):
        reported = getattr(_reconcile_dispatch, "_reported_orphans", set())
        if orphan not in reported:
            location = dispatch_store._root(repo) / orphan
            print("orphan dispatch member %s has no identity; remove it with: rm -r -- %s"
                  % (orphan, shlex.quote(str(location))), file=sys.stderr)
            reported.add(orphan)
            _reconcile_dispatch._reported_orphans = reported
    for member in state["participants"]:
        if not member.get("joined") or member.get("parent") != caller:
            continue
        live = [row for row in rows if row.get("parent_attempt_id") == parent and not row.get("terminal")
                and row.get("refs", {}).get("panel_room_participant") == member["participant"]
                and room.same_room(repo, row.get("refs", {}).get("panel_room_id", room_id), room_id)]
        call = _dispatch_call_record(repo, room_id, member)
        if not call:
            continue
        try:
            released = _dispatch_released(repo, call)
            if (not live and (not member.get("owns") or released)
                    and not _settlement_delivery_pending(repo, room_id, member["participant"])):
                continue
            pending = _dispatch_pending(repo, call)
            task_id = pending["task_id"]
            if pending.get("recovered") and not call.get("task_id") and live:
                task_ids = {row.get("task_id") for row in live}
                if len(task_ids) == 1 and None not in task_ids:
                    task_id = next(iter(task_ids))
            if (any(row.get("task_id") != task_id for row in live)
                    or not _dispatch_call_finished(repo, room_id, member, report_evidence=True)):
                continue
            deliver_result(repo, room_id, member["participant"], caller, task_id, pending["exit"])
        except DispatchEvidenceError as error:
            if released:
                continue
            reported = getattr(_reconcile_dispatch, "_reported_evidence_errors", set())
            key = (error.participant, error.record, error.reason)
            if key not in reported:
                location = dispatch_store._root(repo, error.participant) / error.participant
                print("dispatch reconciliation blocked: invalid %s for %s (%s)"
                      % (error.record, error.participant, error.reason), file=sys.stderr)
                print("Recovery: inspect records with: ls -la -- %s; then retry the dispatch after "
                      "restoring trusted evidence." % shlex.quote(str(location)), file=sys.stderr)
                reported.add(key)
                _reconcile_dispatch._reported_evidence_errors = reported
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            # Missing pending records describe calls still running in another dispatch controller.
            continue


def _dispatch_before_force(before_force):
    if before_force:
        try:
            before_force()
        except (OSError, ValueError):
            print("OMS call pending record unavailable; continuing force-stop.", file=sys.stderr)


def _observe_dispatch_session(process, identity, evidence):
    if (os.name != "posix" or type(process.pid) is not int or process.pid <= 0
            or process.pid in {os.getpid(), os.getpgrp()} or not identity):
        return
    import panel_terminals
    current = panel_terminals.process(process.pid)
    try:
        if (current and current["group"] == process.pid
                and panel_terminals.matches(process.pid, identity) and os.getsid(process.pid) == process.pid):
            evidence["session_leader"] = (process.pid, identity)
    except ProcessLookupError:
        pass


def _stop_dispatch_groups(process, identity, evidence, before_force=None, reaped=True):
    """A reaped session leader can leave foreground and detached provider groups alive."""
    from terminal_panel import _dispatch_exited, _dispatch_recorded_gone
    if os.name != "posix":
        return bool(not evidence.get("forced") and _dispatch_exited(process, identity, reaped=reaped)
                    and not evidence.get("observation_failed")
                and _dispatch_recorded_gone(evidence.get("descendants")))
    if type(process.pid) is not int or process.pid <= 0 or process.pid == os.getpid():
        return False

    def gone():
        from terminal_panel import _dispatch_exited, _dispatch_recorded_gone
        return (_dispatch_exited(process, identity, reaped=reaped)
                and not evidence.get("observation_failed")
                and _dispatch_recorded_gone(evidence.get("descendants")))

    if gone():
        return True
    import panel_terminals

    def signal_groups(number):
        from terminal_panel import _dispatch_before_force, _dispatch_group_gone
        groups, pids = set(), []
        caller_group = os.getpgrp()
        current = panel_terminals.process(process.pid)
        owns_session = (bool(identity) and evidence.get("session_leader") == (process.pid, identity)
                        and (current is None or current.get("identity") == identity))
        # A dead leader cannot supply fresh session proof. Use the identity
        # recorded while it was alive, and never target the caller's group.
        if owns_session and process.pid != caller_group and not _dispatch_group_gone(process.pid):
            groups.add(process.pid)
        records = evidence.get("descendants")
        if isinstance(records, list) and len(records) <= 256:
            for row in records:
                if (isinstance(row, dict) and set(row) == {"pid", "identity", "group"}
                        and type(row["pid"]) is int and row["pid"] > 0 and row["pid"] != os.getpid()
                        and type(row["group"]) is int and row["group"] > 0
                        and isinstance(row["identity"], str) and row["identity"]
                        and panel_terminals.matches(row["pid"], row["identity"])):
                    try:
                        if os.getpgid(row["pid"]) == row["group"]:
                            session = os.getsid(row["pid"])
                            if (row["group"] != caller_group
                                    and (owns_session and session == process.pid
                                         or row["group"] == row["pid"] == session)):
                                groups.add(row["group"])
                            else:
                                pids.append(row)
                    except ProcessLookupError:
                        pass
        if (groups or pids) and not evidence.get("forced"):
            evidence["forced"] = True
            _dispatch_before_force(before_force)
        for group in groups:
            try:
                os.killpg(group, number)
            except ProcessLookupError:
                pass
        for row in pids:
            if row["group"] not in groups:
                panel_terminals.signal_process(row["pid"], row["identity"], number)
        return bool(groups or pids)

    for number, grace in ((signal.SIGTERM, 5), (signal.SIGKILL, 1)):
        if not signal_groups(number):
            return gone()
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            if gone():
                return True
            time.sleep(.05)
    return gone()


def _stop_dispatch(process, identity, term_sent=False, evidence=None, before_force=None, interrupted=False):
    from terminal_panel import _dispatch_before_force, _dispatch_exited, _dispatch_recorded_gone
    from terminal_panel import _observe_dispatch_descendants, _observe_dispatch_session, _stop_dispatch_groups
    evidence = evidence if evidence is not None else {}
    if type(process.pid) is not int or process.pid <= 0 or process.pid == os.getpid():
        return False
    _observe_dispatch_session(process, identity, evidence)
    if type(process.poll()) is int:
        if (_dispatch_exited(process, identity, reaped=True)
                and not evidence.get("observation_failed")
                and _dispatch_recorded_gone(evidence.get("descendants"))):
            return True
        if not term_sent and not interrupted:
            return False
        return _stop_dispatch_groups(process, identity, evidence, before_force)
    try:
        if not term_sent:
            _observe_dispatch_descendants(process, evidence)
            process.terminate()
        status = process.wait(timeout=20)
        if type(status) is not int:
            return False
        return _stop_dispatch_groups(process, identity, evidence, before_force)
    except ProcessLookupError:
        if type(process.poll()) is int:
            return _stop_dispatch_groups(process, identity, evidence, before_force)
        return (_dispatch_exited(process, identity)
                and not evidence.get("observation_failed")
                and _dispatch_recorded_gone(evidence.get("descendants")))
    except subprocess.TimeoutExpired:
        # The delegate's TERM trap owns worktree cleanup; force only its original session after that grace.
        if os.name == "posix":
            import panel_terminals
            current = panel_terminals.process(process.pid)
            if (not identity or not current or current["group"] != process.pid
                    or evidence.get("session_leader") != (process.pid, identity)
                    or process.pid == os.getpgrp()
                    or not panel_terminals.matches(process.pid, identity)):
                print("OMS call shutdown could not verify the child identity; force-stop skipped.", file=sys.stderr)
                return _stop_dispatch_groups(process, identity, evidence, before_force, reaped=False)
            try:
                _observe_dispatch_descendants(process, evidence)
                evidence["forced"] = True
                _dispatch_before_force(before_force)
                if not panel_terminals.matches(process.pid, identity):
                    return False
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            evidence["forced"] = True
            _dispatch_before_force(before_force)
            process.kill()
        status = process.wait(timeout=5)
        if type(status) is not int:
            return False
        return _stop_dispatch_groups(process, identity, evidence, before_force)


def _settle_dispatch(repo, room_id, member, status):
    from terminal_panel import _dispatch_call_finished, _dispatch_call_record, _dispatch_exit_record, events
    from terminal_panel import update_activity
    participant = room.participant(room.status(repo, room_id), member)
    if not _dispatch_call_finished(repo, room_id, participant):
        raise ValueError("dispatch containment receipt unavailable")
    call = _dispatch_call_record(repo, room_id, participant)
    if not call:
        raise ValueError("dispatch launch changed during settlement")
    stale = any(dispatch_store.stale_boot(repo, member + suffix)
                for suffix in ("-launch.json", "-process.json", "-pending.json", "-supervisor.json"))
    if stale:
        pending = _dispatch_pending(repo, call)
        exit_status = _dispatch_stale_exit(repo, call, pending)
        status = 143 if exit_status is None else exit_status
    rows = json.loads(events(repo, "list", "--json", output=True))
    for row in rows:
        refs = row.get("refs", {})
        if (refs.get("panel_room_participant") == member and room.same_room(repo, refs.get("panel_room_id", room_id), room_id)
                and row.get("state") in {"starting", "working", "verifying"}):
            update_activity(repo, "transition", "--attempt", row["attempt_id"],
                            "--state", "cancelled" if status in {129, 130, 143} else "failed",
                            "--reason-code", "panel_call_interrupted", "--actor", "panel-dispatch")
    # Room scope declarations belong to their participant. Mark controller release
    # privately instead of impersonating the child to mutate its declaration.
    dispatch_store.write(repo, member + "-settled.json",
                         {"released": True, "exit": status,
                          "launch_sha256": _dispatch_exit_record(call, 0)["launch_sha256"]})


def _rollback_dispatch_enrollment(repo, room_id, member):
    # Until Popen succeeds the controller owns this unstarted identity. Bind only
    # the cleanup subprocess; ordinary leave/session and harness-child guards apply.
    from terminal_panel import ENTRY, clean
    env = dict(os.environ, OMS_ROOM_ID=room_id, OMS_ROOM_REPO=str(repo), OMS_ROOM_PARTICIPANT=member)
    result = subprocess.run(["bash", str(ENTRY), "room", "leave", "--repo", str(repo),
                             "--id", room_id, "--participant", member], env=env,
                            capture_output=True, text=True, timeout=15, stdin=subprocess.DEVNULL)
    if result.returncode:
        raise ValueError(clean(result.stderr or result.stdout, 300))


def run_dispatch(repo, owner, role, access, prompt, brief, verify, task_id,
                 dry_run, label, route, main, resume, scopes, work_repo=None, member_id=None, fallback=None, alias=None,
                 candidate=None, continuation_patch=None, continue_call=False, resume_usage_baseline=None):
    from terminal_panel import ENTRY, NativeShutdown, WRITERS, _dispatch_call_finished, _dispatch_capacity
    from terminal_panel import _dispatch_exit_record, _dispatch_output_log, _dispatch_supervisor_receipt
    from terminal_panel import _observe_dispatch_descendants, _observe_dispatch_session
    from terminal_panel import _prepare_dispatch_call, _print_dispatch_next_ready, _read_dispatch_brief
    from terminal_panel import _reconcile_dispatch, _rollback_dispatch_enrollment, _rollover_if_needed
    from terminal_panel import _safe_patch_path, _save_dispatch_output, _stop_dispatch, _tee_dispatch_output
    from terminal_panel import _validate_dispatch_verifier, _validate_dry_run_inputs, active_mains
    from terminal_panel import child_environment, deliver_result, events, moved_main, native_repository
    from terminal_panel import plan_link, repository, routed_command, scope_overlaps, snapshot
    from terminal_panel import work_repository
    source_repo = repo
    if os.environ.get("OMS_ROOM_REPO"):
        repo = repository(os.environ["OMS_ROOM_REPO"])
    execution = repo
    if work_repo is not None or source_repo != repo:
        execution = work_repository(work_repo or source_repo)
    elif route["kind"] in {"worker", "researcher"}:
        execution = native_repository(repo)
    if candidate is None:
        _validate_dispatch_verifier(execution, verify)
    # Candidate suite/selector validation runs after materialization in peer-review.
    if dry_run:
        _validate_dry_run_inputs(prompt, brief)
    if task_id is None:
        task_id = "panel-" + route["purpose"] + "-" + uuid.uuid4().hex[:8]
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", task_id):
        raise ValueError("invalid task ID")
    argv = routed_command(ENTRY, repo, route, prompt, brief, verify, task_id, resume)
    argv[argv.index("--repo") + 1] = str(execution)
    if candidate is not None:
        if (role != "reviewer" or access != "read" or not verify or
                not isinstance(candidate, dict) or set(candidate) != {"patch", "sha256", "base"} or
                not _safe_patch_path(candidate.get("patch")) or
                not candidate["patch"].startswith(".oms/artifacts/") or
                not re.fullmatch(r"[0-9a-f]{64}", str(candidate.get("sha256"))) or
                not re.fullmatch(r"[0-9a-f]{7,64}", str(candidate.get("base")))):
            raise ValueError("automatic review requires a linked patch digest and recorded base")
        argv += ["--candidate-patch", candidate["patch"], "--candidate-sha256", candidate["sha256"],
                 "--candidate-base", candidate["base"]]
    plan = {"schema": 1, "kind": "oms-panel-route", "route": route, "task_id": task_id,
            "argv": argv, "executes": False}
    if scopes:
        if role not in WRITERS or access != "write":
            raise ValueError("--scope declares a write role's files; use it with --access write")
        scopes = list(dict.fromkeys(room.normalize_scope(scope) for scope in scopes))
        room.declared_scopes(route["kind"], scopes)
    if dry_run and not main and not (scopes and os.environ.get("OMS_ROOM_ID")):
        print(json.dumps(dict(plan, overlaps=[]) if scopes else plan))
        return 0
    if not dry_run and not shutil.which(route.get("binary", route["provider"])):
        raise ValueError("%s is not installed; this exact role route cannot run" % route["provider"])
    env = child_environment(owner, repo)
    env.pop("OMS_RESUME_USAGE_BASELINE", None)
    if resume_usage_baseline:
        env["OMS_RESUME_USAGE_BASELINE"] = resume_usage_baseline
    if continuation_patch:
        env["OMS_PANEL_CONTINUATION"] = json.dumps(continuation_patch, separators=(",", ":"))
    if access == "write" and not env.get("OMS_PEER_TIMEOUT"):
        # A scoped build plus its own checks outlasts the 30m delegate wall; high effort gets the longest.
        env["OMS_PEER_TIMEOUT"] = "60m" if route.get("effort") == "high" else "40m"
    if verify and not env.get("OMS_PEER_VERIFY_TIMEOUT"):
        # Repository suites such as the operator smoke outlast the 10m delegate verify default.
        env["OMS_PEER_VERIFY_TIMEOUT"] = "20m"
    parent = env.get("OMS_PANEL_MAIN_ATTEMPT")
    if parent:
        record = json.loads(events(repo, "show", "--attempt", parent, "--json", output=True))
        if record.get("tool") != "panel-main" or record.get("provider") != owner or record.get("terminal"):
            raise ValueError("panel main identity does not match this owner")
    moved = moved_main(repo, owner, env)
    if moved:
        env["OMS_ROOM_ID"] = moved[0]
    env.pop("OMS_ATTEMPT_ID", None)
    env.update(OMS_PANEL_DISPATCH="1", OMS_PANEL_ROLE=route["kind"],
               OMS_PANEL_PURPOSE=route["purpose"], OMS_PANEL_WORKLOAD=route["workload"])
    if fallback:
        env.update(OMS_PANEL_FALLBACK_PROVIDER=fallback["provider"],
                   OMS_PANEL_FALLBACK_MODEL=fallback["model"],
                   OMS_PANEL_FALLBACK_EFFORT=fallback["effort"] or "")
    else:
        for name in ("OMS_PANEL_FALLBACK_PROVIDER", "OMS_PANEL_FALLBACK_MODEL", "OMS_PANEL_FALLBACK_EFFORT"):
            env.pop(name, None)
    if label:
        env["OMS_PANEL_LABEL"] = label
    else:
        env.pop("OMS_PANEL_LABEL", None)
    if alias:
        env["OMS_PANEL_ALIAS"] = alias
    env["OMS_TASK_ID"] = task_id
    room_id = room.resolve(repo, env.get("OMS_ROOM_ID"))
    if room_id:
        # An id that rolled over names its successor; the worker and plan binding get the current room.
        env["OMS_ROOM_ID"] = room_id
    if main and not room_id:
        raise ValueError("--main needs a room; select it with --room")
    if not room_id and not parent:
        print("Standalone dispatch: room and main relationship are unrecorded; plan protection is absent.", file=sys.stderr)
    if member_id is not None and not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", member_id):
        raise ValueError("invalid panel call identifier")
    member = None
    caller = env.get("OMS_ROOM_PARTICIPANT")
    if room_id:
        state = room.project(room.records(repo, room_id))
        scope_review_state = None
        caller = caller or (record.get("refs", {}).get("panel_room_participant", parent) if parent
                            else moved[1] if moved else None)
        candidates = active_mains(repo, room_id, state, owner, caller=caller, attempt=parent)
        if len(candidates) != 1:
            raise ValueError("select a unique active main in this room before dispatching")
        caller, record = next(iter(candidates.items()))
        parent = env["OMS_PANEL_MAIN_ATTEMPT"] = record["attempt_id"]
        if main and caller != main:
            raise ValueError("--main names a different main than this caller; dispatch from that main's own session")
        member_state = room.participant(state, caller)
        if member_state["role"] != "main" or member_state["provider"] != owner:
            raise ValueError("room caller must be a main of the selected owner provider")
        if parent and record.get("refs", {}).get("panel_room_participant", parent) != caller:
            raise ValueError("panel main and room caller identities disagree")
        if parent and not room.same_room(repo, record.get("refs", {}).get("panel_room_id", room_id), room_id):
            raise ValueError("panel main and selected room identities disagree")
        # Refuse before rollover or reconciliation can seal a room or deliver results.
        if not dry_run and access == "write" and continuation_patch is None:
            from review_backlog import warning_for
            try:
                report = snapshot(repo, room_id)[0]
                notice = warning_for(report, caller)
                report_room = report.get("room", {}) if isinstance(report, dict) else {}
                finalized = report.get("finalized") if isinstance(report, dict) else None
                if (isinstance(report_room, dict) and room.same_room(repo, report_room.get("id"), room_id)
                        and isinstance(finalized, dict)):
                    scope_review_state = {
                        "call_results": report_room.get("call_results", {}),
                        "room_attempts": report.get("room_attempts", {}),
                        "finalized": finalized,
                    }
            except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
                notice = None  # throughput policy fails open; an unreadable backlog never blocks work
            if notice:
                raise ValueError(notice + "; admit or reject each result, or settle it with oms panel --finalize "
                                 "--owner %s --task-id ID, or send findings back with --continue" % owner)
        if not dry_run and parent:
            moved_to = _rollover_if_needed(repo, room_id, state, caller)
            if moved_to:
                room_id = env["OMS_ROOM_ID"] = moved_to
                state = room.project(room.records(repo, room_id))
            _dispatch_capacity(room_id, state)
            _reconcile_dispatch(repo, room_id, state, caller, parent)
            state = room.project(room.records(repo, room_id))
        if scopes:
            overlap_state = dict(state)
            if scope_review_state is not None:
                overlap_state["scope_review_state"] = scope_review_state
            found = scope_overlaps(repo, room_id, overlap_state, caller, scopes)
            if found:
                print("Scope overlap (coordinate before editing): " + ", ".join(
                    "%s %s%s" % (item["participant"], item["scope"],
                                  " (" + item["status"] + ")" if item.get("status") else "")
                    for item in found), file=sys.stderr)
            if dry_run:
                print(json.dumps(dict(plan, overlaps=found)))
                return 0
        if dry_run:
            print(json.dumps(plan))
            return 0
        _dispatch_capacity(room_id, state)
    if access == "write":
        # Delegate's dry-run validates the same brief and route without starting a worker.
        # Give it no room/plan identity so a refused preview cannot claim or enroll this call.
        preview = list(argv)
        preview[preview.index("--repo") + 1] = str(execution)
        preview[preview.index("--task-id", preview.index("--verify") + 2) + 1] = "preflight-" + uuid.uuid4().hex[:16]
        preview_env = {name: value for name, value in env.items()
                       if not name.startswith(("OMS_ROOM_", "OMS_PANEL_", "OMS_OBSERVE_PLAN_", "OMS_DL_"))
                       and name not in {"OMS_TASK_ID", "OMS_ATTEMPT_ID", "OMS_PARENT_ATTEMPT_ID",
                                        "OMS_PLAN_ID", "OMS_PLAN_TASK_ID", "OMS_LEASE_ID", "OMS_PLAN_LEASE_ID"}}
        # Keep canonical artifact routing, but not the caller or child identity.
        preview_env.update(OMS_PANEL_DISPATCH="1", OMS_PANEL_REPO=str(repo), OMS_ROOM_REPO=str(repo))
        if continuation_patch:
            preview_env["OMS_PANEL_CONTINUATION"] = env["OMS_PANEL_CONTINUATION"]
        checked = subprocess.run(preview + ["--dry-run"], cwd=str(execution), env=preview_env,
                                 capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=120)
        if checked.returncode:
            print("Delegate preflight refused before task claim or worker enrollment.", file=sys.stderr)
            if checked.stderr:
                print(checked.stderr.rstrip(), file=sys.stderr)
            return checked.returncode
    # Observe and claim with the caller's main identity before enrolling a child.
    # A joined child must never become the apparent owner of its own plan task.
    binding = plan_link(repo, owner, task_id, env, caller, access)
    if binding and route["kind"] in {"worker", "researcher"}:
        argv += ["--observe-plan-binding", json.dumps(binding._asdict(), separators=(",", ":"))]
    if access == "write":
        # The worker's first prompt states its path contract: the plan task's
        # stored paths and the scopes this dispatch declared.
        allowed, forbidden = _plan_task_paths(repo, task_id)
        allowed = list(dict.fromkeys(allowed + list(scopes or [])))
        if allowed:
            env["OMS_PANEL_ALLOWED_PATHS"] = ", ".join(allowed)
        if forbidden:
            env["OMS_PANEL_FORBIDDEN_PATHS"] = ", ".join(forbidden)
    if room_id:
        if not continue_call or alias is None:
            attempts = json.loads(events(repo, "list", "--json", output=True))
            call_rows = [row for row in attempts
                         if row.get("refs", {}).get("panel_alias")
                         and room.same_room(repo, row.get("refs", {}).get("panel_room_id"), room_id)]
            alias = _reserve_alias(repo, room_id, role, alias, call_rows)
        member = member_id or "call-" + uuid.uuid4().hex[:16]
        room.join(repo, room_id, member, route["provider"], route["kind"], route["model"], label or route["purpose"],
                  owns=scopes, parent=caller, dispatch_role=route["role"], purpose=route["purpose"], state="queued",
                  alias=alias)
        try:
            # A rollover that sealed the room meanwhile carried the join; enroll the call where it now lives.
            room_id = room.settle(repo, room_id)
            delivery_state = room.project(room.records(repo, room_id))
            if delivery_state["closed"]:
                raise ValueError("room is closed")
            room.participant(delivery_state, caller)
            room.participant(delivery_state, member)
            room.send(repo, room_id, caller, member, "Assigned: " + (label or route["purpose"]), "handoff")
        except BaseException:
            try:
                _rollback_dispatch_enrollment(repo, room_id, member)
            except (OSError, ValueError, subprocess.SubprocessError):
                print("OMS room assignment rollback unavailable.", file=sys.stderr)
            raise
        env.update(OMS_ROOM_ID=room_id, OMS_ROOM_PARTICIPANT=member, OMS_ROOM_REPO=str(repo),
                   OMS_ROOM_ADMITTED_PARTICIPANT=member)
        if access == "write" and route["kind"] == "worker":
            env["OMS_ROOM_RELAY_ALLOWED"] = "1"
    elif alias is None:
        alias = ROLE_ALIAS_LETTERS.get(role, "W") + "1"
    if alias:
        env["OMS_PANEL_ALIAS"] = alias
    argv[argv.index("--repo") + 1] = str(execution)
    print("%s / %s -> %s / %s / %s / %s" % (owner, role, route["provider"], route["model"],
                                            route["effort"] or "native-settings", access) +
          " task_id=" + task_id, flush=True)
    status, process, identity = 1, None, None
    launch = None
    control_member = member or "call-" + uuid.uuid4().hex[:16]
    env["OMS_PANEL_CALL_ID"] = control_member
    spawn_failed = False
    term_sent = False
    shutdown = None
    evidence = {}
    saved_handlers = {}

    def record_pending():
        from terminal_panel import _record_dispatch_pending
        if not launch:
            raise ValueError("dispatch process record unavailable")
        _record_dispatch_pending(repo, launch, status, task_id, evidence)

    def interrupt(signum, frame):
        from terminal_panel import NativeShutdown, _observe_dispatch_descendants
        nonlocal term_sent
        for number in saved_handlers:
            signal.signal(number, signal.SIG_IGN)
        if process and process.poll() is None:
            try:
                # Delegate disables its traps during cleanup; a second TERM would abort that cleanup.
                _observe_dispatch_descendants(process, evidence)
                process.terminate()
                term_sent = True
            except OSError:
                pass
        raise NativeShutdown(signum)

    try:
        for name in ("SIGINT", "SIGTERM", "SIGHUP"):
            number = getattr(signal, name, None)
            if number is not None:
                saved_handlers[number] = signal.signal(number, interrupt)
        try:
            recorded_brief = Path(brief).expanduser() if access == "write" else None
            if recorded_brief is not None and not recorded_brief.is_absolute():
                recorded_brief = repo / recorded_brief
            rework = (dict(role=role, scope=scopes or [], workload=route["workload"],
                           brief=_read_dispatch_brief(recorded_brief).decode("utf-8")) if access == "write" else None)
            launch = _prepare_dispatch_call(repo, room_id, control_member, caller, verify, execution, rework,
                                             task_id=task_id)
            env["XDG_STATE_HOME"] = dispatch_store.pinned_state_home(repo, control_member)
            command = argv
            if launch and launch["backend"] == "subreaper":
                digest = _dispatch_exit_record(launch, 0)["launch_sha256"]
                command = [sys.executable, str(Path(__file__).with_name("dispatch_supervisor.py")),
                           str(repo), control_member, digest] + argv
            output_log, output_fd = _dispatch_output_log(repo, control_member)
            process = subprocess.Popen(command, cwd=str(execution), env=env, start_new_session=os.name == "posix",
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            output_tail, output_lock = bytearray(), threading.Lock()
            output_threads = [threading.Thread(target=_tee_dispatch_output,
                                               args=(getattr(process, channel, None), destination,
                                                     output_tail, output_lock), daemon=True)
                              for channel, destination in (("stdout", sys.stdout), ("stderr", sys.stderr))]
            for output_thread in output_threads:
                output_thread.start()
            if launch:
                binding = {"pid": process.pid}
                if os.name == "posix":
                    import panel_terminals
                    supervisor = panel_terminals.process(process.pid)
                    if supervisor and supervisor["group"] == process.pid:
                        binding.update(identity=supervisor["identity"], group=supervisor["group"])
                dispatch_store.write(repo, control_member + "-process.json", binding, exclusive=True)
        except (OSError, ValueError):
            spawn_failed = process is None
            if process is None and "output_fd" in locals():
                os.close(output_fd)
            if process is not None and process.poll() is None:
                process.terminate()
                term_sent = True
            raise
        if os.name == "posix":
            import panel_terminals
            current = panel_terminals.process(process.pid)
            identity = current["identity"] if current else None
            _observe_dispatch_session(process, identity, evidence)
        while True:
            _observe_dispatch_descendants(process, evidence)
            if launch and launch["backend"] == "subreaper":
                try:
                    receipt = _dispatch_supervisor_receipt(repo, launch)
                    if receipt and receipt["exit"] is not None:
                        status = receipt["exit"]
                        break
                except (OSError, ValueError, KeyError, TypeError):
                    pass
            try:
                status = process.wait(timeout=1)
                break
            except subprocess.TimeoutExpired:
                pass
        if status < 0:
            status = 128 - status
    except NativeShutdown as error:
        status = error.exit_code
        if status != 130:
            shutdown = error
    except KeyboardInterrupt:
        status = 130
    finally:
        # Repeated cancellation must not abort cleanup or the result handoff.
        for number in saved_handlers:
            signal.signal(number, signal.SIG_IGN)
        try:
            exit_verified = False
            try:
                if process and launch:
                    if (term_sent or status in {129, 130, 143}) and process.poll() is None:
                        if not term_sent:
                            process.terminate()
                        try:
                            process.wait(timeout=25)
                        except subprocess.TimeoutExpired:
                            pass
                    # Snapshots may assist shutdown but never establish whole-tree exhaustion.
                    record_pending()
                    exit_verified = _dispatch_call_finished(repo, room_id, launch)
                elif process:
                    exit_verified = _stop_dispatch(process, identity, term_sent, evidence, record_pending,
                                                   interrupted=status in {129, 130, 143})
            except (OSError, ValueError, subprocess.SubprocessError):
                print("OMS call shutdown unavailable.", file=sys.stderr)
            if "output_threads" in locals():
                for output_thread in output_threads:
                    output_thread.join(timeout=5)
                _save_dispatch_output(output_fd, output_tail)
            if member and exit_verified:
                try:
                    record_pending()
                except (OSError, ValueError):
                    exit_verified = False
                    print("OMS call exit record unavailable; non-terminal scopes remain reserved.", file=sys.stderr)
            if member and exit_verified:
                deliver_result(repo, room_id, member, caller, task_id, status)
            elif member and process is None and spawn_failed:
                try:
                    _rollback_dispatch_enrollment(repo, room_id, member)
                except (OSError, ValueError, subprocess.SubprocessError):
                    print("OMS room assignment rollback unavailable.", file=sys.stderr)
            elif member:
                if status == 0:
                    status = 1
                try:
                    record_pending()
                except (OSError, ValueError):
                    print("OMS call pending record unavailable; owner cleanup is required.", file=sys.stderr)
                print("OMS call settlement pending: child exit is unverified; attempt and scope retained.", file=sys.stderr)
                print("Manual clear after independently stopping every provider descendant: OMS_ROOM_PARTICIPANT= oms room scope --repo " +
                      shlex.quote(str(repo)) + " --id " + room_id + " --participant " + member + " --clear", file=sys.stderr)
                print("Cancel matching attempts with oms agent-events --repo " + shlex.quote(str(repo)) +
                      " transition --attempt ATTEMPT_ID --state cancelled --reason-code manual_clear; " +
                      "then oms room leave --repo " + shlex.quote(str(repo)) + " --id " + room_id +
                      " --participant " + member + ".", file=sys.stderr)
            elif process and not exit_verified:
                status = status or 1
                print("OMS call settlement pending: whole-tree termination is unverified; manually stop all provider descendants.",
                      file=sys.stderr)
        finally:
            for number, previous in saved_handlers.items():
                signal.signal(number, previous)
    if shutdown is not None:
        raise shutdown
    if exit_verified:
        _print_dispatch_next_ready(repo, owner)
    return status


def _print_dispatch_next_ready(repo, owner):
    from hook_state import panel_next_ready_hint, plan_tasks

    try:
        hint = panel_next_ready_hint(plan_tasks(repo)[1], owner)
        if hint:
            print("[oms plan] " + hint, file=sys.stderr)
    except (OSError, ValueError, KeyError, TypeError):
        pass


def council_command(repo, prompt, task_id, label, thread=None, rounds=1, workload="deep"):
    from terminal_panel import ENTRY, MAX_PROMPT, policy, safe_label
    if not 1 <= rounds <= 2:
        raise ValueError("council rounds must be 1 or 2")
    if not task_id or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", task_id):
        raise ValueError("council needs a valid task ID")
    if not prompt or len(prompt.encode("utf-8")) > MAX_PROMPT or "\0" in prompt:
        raise ValueError("council needs a bounded prompt")
    if not safe_label(label):
        raise ValueError("council needs a task title")
    routes = policy()
    # Four seats, two per family: deep plus a cheaper routine pair by default; frontier on request.
    seats = routes["tiers"]["deep"] + routes["tiers"]["frontier" if workload == "frontier" else "routine"]
    if len({item["provider"] for item in seats}) < 2:
        raise ValueError("council tier must include two independent provider families")
    targets = ",".join(item["provider"] + ":model=" + item["model"] for item in seats)
    argv = ["bash", str(ENTRY), "peer-ask", "--repo", str(repo),
            "--providers", targets, "--debate", str(rounds), "--deliberation",
            "--require-complete", "--no-task", "--no-memory", "--reasoning-effort", "high", "--prompt", prompt]
    thread = thread or "panel-council-" + uuid.uuid5(uuid.NAMESPACE_URL, task_id).hex[:24]
    if thread:
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", thread):
            raise ValueError("invalid council thread ID")
        argv += ["--thread", thread]
    return argv


def council(repo, owner, task_id, label, prompt, thread=None, rounds=1, dry_run=False, workload="deep"):
    from terminal_panel import PROVIDERS, active_mains, child_environment, council_command, events, safe_label
    from terminal_panel import update_activity
    if owner not in PROVIDERS:
        raise ValueError("choose a supported council owner")
    argv = council_command(repo, prompt, task_id, label, thread, rounds, workload)
    # Advertise the seats actually launched: route tiers may hold fewer or more than two entries.
    seats = argv[argv.index("--providers") + 1].split(",")
    families = len({seat.split(":", 1)[0] for seat in seats})
    primary_calls = len(seats) * (rounds + 1)
    if dry_run:
        print(json.dumps({"schema": 1, "kind": "oms-panel-council-plan", "argv": argv,
                          "seats": len(seats), "families": families, "primary_calls": primary_calls,
                          "max_format_repairs": primary_calls, "max_calls": primary_calls * 2,
                          "executes": False}))
        return 0
    env = child_environment(owner, repo)
    parent = env.get("OMS_PANEL_MAIN_ATTEMPT")
    room_id = room.resolve(repo, env.get("OMS_ROOM_ID"))
    if room_id:
        found = active_mains(repo, room_id, room.project(room.records(repo, room_id)), owner,
                             caller=env.get("OMS_ROOM_PARTICIPANT"), attempt=parent)
        if len(found) != 1:
            raise ValueError("select a unique active main in this room before starting a council")
        selected = next(iter(found.values()))
        if parent and selected["attempt_id"] != parent:
            raise ValueError("panel main identity does not match an active owner")
        parent = selected["attempt_id"]
    where = {}
    if parent:
        record = json.loads(events(repo, "show", "--attempt", parent, "--json", output=True))
        if record.get("tool") != "panel-main" or record.get("provider") != owner or record.get("terminal"):
            raise ValueError("panel main identity does not match an active owner")
        refs_ = record.get("refs") or {}
        where = {k: refs_[k] for k in ("panel_room_id", "panel_room_participant") if refs_.get(k)}
    where.setdefault("panel_room_id", room_id or "")
    where.setdefault("panel_room_participant", env.get("OMS_ROOM_PARTICIPANT") or "")
    env.pop("OMS_ATTEMPT_ID", None)
    env.update(OMS_TASK_ID=task_id, OMS_PANEL_DISPATCH="1", OMS_PANEL_ROLE="advisor",
               OMS_PANEL_PURPOSE="advise", OMS_PANEL_WORKLOAD="main",
               OMS_PANEL_LABEL=safe_label(label))
    print("council: %s seats / %s families / %s primary calls + up to %s format repairs / %s" %
          (len(seats), families, primary_calls, primary_calls,
           ", ".join(seat.split(":model=", 1)[1] for seat in seats)), flush=True)
    refs = ["start", "--provider", owner, "--tool", "panel-council", "--task-id", task_id,
            "--ref", "panel_role=advisor", "--ref", "panel_owner=" + owner,
            "--ref", "panel_label=" + safe_label(label), "--ref", "panel_purpose=advise",
            "--ref", "panel_location=repository", "--ref", "panel_thread=" + argv[-1],
            "--then", "starting", "--then", "working"]
    for key, value in where.items():
        if value:
            refs += ["--ref", key + "=" + value]
    if parent:
        refs += ["--parent-attempt-id", parent]
    attempt = events(repo, *refs, output=True)
    status, process = 1, None
    try:
        process = subprocess.Popen(argv, cwd=str(repo), env=env)
        while True:
            try:
                status = process.wait(timeout=60)
                break
            except subprocess.TimeoutExpired:
                update_activity(repo, "heartbeat", "--attempt", attempt, "--actor", "panel-council")
    except KeyboardInterrupt:
        status = 130
        raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=5)
        if status == 0:
            update_activity(repo, "transition", "--attempt", attempt, "--state", "verifying", "--then", "review",
                   "--then", "done", "--reason-code", "council_complete")
        else:
            update_activity(repo, "transition", "--attempt", attempt, "--state", "cancelled" if status == 130 else "failed",
                   "--reason-code", "council_incomplete")
    return status


def peer_command(action, provider, repo, prompt=None, brief=None, verify=None):
    from terminal_panel import ENTRY
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
    from terminal_panel import child_environment
    return subprocess.call(command, cwd=str(repo),
                           env=child_environment(provider, repo) if provider else None)


def dashboard(repo, as_json=False):
    from terminal_panel import ENTRY
    command = ["bash", str(ENTRY), "dashboard", "--repo", str(repo)]
    if as_json:
        command += ["--json"]
    result = subprocess.run(command, cwd=str(repo), capture_output=True,
                            text=True, timeout=30, check=False, stdin=subprocess.DEVNULL)
    if as_json:
        return json.loads(result.stdout), result.returncode
    return result.stdout, result.returncode
