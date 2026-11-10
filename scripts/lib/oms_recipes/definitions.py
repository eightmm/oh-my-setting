"""Strict recipe/workspace contracts and deterministic, shell-free argv expansion."""

from __future__ import annotations

import copy
import json
import math
import os
from pathlib import Path
import re
import shutil
from typing import Any

from oms_runtime.common import CoreError, SECRET_NAME_RE, SECRET_VALUE_RE, read_bytes, sensitive_text, sha256_bytes
from .common import (
    HEX, MAX_STEPS, bounded_int, digest, identifier,
    load_definition, object_fields, relative, text, under,
)

TOKEN_RE = re.compile(r"\$\{(input|port|resource)\.([A-Za-z][A-Za-z0-9_-]{0,63})\}")
ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$")


def _array(value: Any, label: str, maximum: int, *, empty: bool = True) -> list:
    if not isinstance(value, list) or len(value) > maximum or (not empty and not value):
        raise CoreError(label + " must be a bounded array")
    return value


def _mapping(value: Any, label: str, maximum: int = 64) -> dict:
    if not isinstance(value, dict) or len(value) > maximum:
        raise CoreError(label + " must be a bounded object")
    for key in value:
        identifier(key, label + " name", name=True)
    return value


def _template(value: Any, label: str) -> str:
    value = text(value, label, empty=True)
    if "${" in TOKEN_RE.sub("", value):
        raise CoreError(label + " contains an unsupported interpolation")
    return value


def _environment(value: Any) -> dict:
    if not isinstance(value, dict) or len(value) > 64:
        raise CoreError("environment must be a bounded object")
    for name, entry in value.items():
        if not ENV_NAME.fullmatch(name) or SECRET_NAME_RE.search(name):
            raise CoreError("invalid or sensitive environment name")
        if name.startswith("OMS_") or name in {"PATH", "HOME", "PYTHONPATH", "PYTHONHOME", "BASH_ENV", "ENV", "SHELLOPTS"} or name.startswith(("LD_", "DYLD_")):
            raise CoreError("environment cannot override execution or authority controls")
        object_fields(entry, {"value", "from_env"}, set(), "environment entry")
        if len(entry) != 1:
            raise CoreError("environment entries require exactly one of value/from_env")
        if "value" in entry:
            _template(entry["value"], "environment value")
        else:
            ref = entry["from_env"]
            if not isinstance(ref, str) or not ENV_NAME.fullmatch(ref) or SECRET_NAME_RE.search(ref):
                raise CoreError("invalid or sensitive environment reference")
    return value


def _inputs(value: Any) -> dict:
    for name, spec in _mapping(value, "inputs").items():
        object_fields(spec, {"type", "default", "required", "choices", "min", "max", "max_length"}, {"type"}, "input")
        if SECRET_NAME_RE.search(name):
            raise CoreError("sensitive input names are unsupported")
        if spec["type"] not in {"string", "integer", "number", "boolean", "path", "enum"}:
            raise CoreError("unsupported input type")
        if "required" in spec and type(spec["required"]) is not bool:
            raise CoreError("input required must be boolean")
        if spec.get("required") is False and "default" not in spec:
            raise CoreError("optional inputs require a default")
        if spec["type"] == "enum":
            choices = _array(spec.get("choices"), "enum choices", 64, empty=False)
            if any(not isinstance(item, str) for item in choices) or len(set(choices)) != len(choices):
                raise CoreError("enum choices must be unique strings")
            for item in choices:
                text(item, "enum choice")
        elif "choices" in spec:
            raise CoreError("choices applies only to enum inputs")
        if any(key in spec for key in ("min", "max")):
            if spec["type"] not in {"integer", "number"}:
                raise CoreError("min/max apply only to numeric inputs")
            for key in ("min", "max"):
                bound = spec.get(key, 0)
                if type(bound) not in (int, float) or (type(bound) is int and not -(2**63) <= bound < 2**63) or (type(bound) is float and not math.isfinite(bound)):
                    raise CoreError("numeric bounds must be finite")
            if spec.get("min", -math.inf) > spec.get("max", math.inf):
                raise CoreError("input min exceeds max")
        if "max_length" in spec:
            bounded_int(spec["max_length"], "max_length", 1, 4096)
        if "default" in spec:
            input_value(spec["default"], spec)
    return value


def input_value(value: Any, spec: dict) -> Any:
    kind = spec["type"]
    if kind in {"string", "path", "enum"}:
        text(value, "input value", spec.get("max_length", 4096), empty=kind == "string")
        if sensitive_text(value):
            raise CoreError("inputs cannot retain private absolute paths or sensitive material")
        if kind == "path":
            relative(value, "path input", dot=True)
        if kind == "enum" and value not in spec["choices"]:
            raise CoreError("input is outside its enum choices")
    elif kind == "boolean":
        if type(value) is not bool:
            raise CoreError("boolean input must be true or false")
    elif kind in {"integer", "number"}:
        types = (int,) if kind == "integer" else (int, float)
        if type(value) not in types or (type(value) is int and not -(2**63) <= value < 2**63) or (type(value) is float and not math.isfinite(value)):
            raise CoreError("numeric input has an invalid type or nonfinite value")
        if not spec.get("min", -math.inf) <= value <= spec.get("max", math.inf):
            raise CoreError("numeric input is outside its bounds")
    return value


def bind_inputs(specs: dict, supplied: dict, *, cli: bool = False) -> dict:
    if supplied.keys() - specs.keys():
        raise CoreError("unknown recipe input")
    result = {}
    for name, spec in specs.items():
        if name in supplied:
            value = supplied[name]
            if cli and spec["type"] in {"integer", "number", "boolean"}:
                try:
                    value = json.loads(value)
                except (ValueError, TypeError):
                    raise CoreError("typed input requires JSON number or boolean: " + name)
        elif "default" in spec:
            value = spec["default"]
        else:
            raise CoreError("missing required input: " + name)
        result[name] = input_value(value, spec)
    return result


def _outputs(value: Any) -> list:
    for row in _array(value, "outputs", 64):
        object_fields(row, {"path", "min_bytes", "max_bytes", "sha256", "fresh"}, {"path"}, "output requirement")
        _template(row["path"], "output path")
        minimum = bounded_int(row.get("min_bytes", 0), "output min_bytes", 0, 64 * 1024 * 1024)
        maximum = bounded_int(row.get("max_bytes", 16 * 1024 * 1024), "output max_bytes", 1, 64 * 1024 * 1024)
        if minimum > maximum:
            raise CoreError("output min_bytes exceeds max_bytes")
        if "sha256" in row and (not isinstance(row["sha256"], str) or not HEX.fullmatch(row["sha256"])):
            raise CoreError("output sha256 must be a lowercase SHA-256")
        if "fresh" in row and type(row["fresh"]) is not bool:
            raise CoreError("output fresh must be boolean")
    return value


def _commands(value: Any) -> dict:
    for name, command in _mapping(value, "commands").items():
        object_fields(command, {"argv", "cwd", "environment", "depends", "timeout_seconds", "max_output_bytes"}, {"argv"}, "command")
        for arg in _array(command["argv"], "argv", 128, empty=False):
            _template(arg, "argv item")
        executable = command["argv"][0]
        if not executable or "${input." in executable or "${port." in executable:
            raise CoreError("the executable must be a declared command or pack resource")
        interpreters = {"sh", "bash", "dash", "zsh", "ksh", "python", "python3", "node", "ruby", "perl"}
        if Path(executable).name in interpreters:
            for index, arg in enumerate(command["argv"][:-1]):
                if arg in {"-c", "-e", "--eval"} and TOKEN_RE.search(command["argv"][index + 1]):
                    raise CoreError("interpreter code cannot interpolate inputs; pass them as separate argv items")
        _template(command.get("cwd", "."), "command cwd")
        _environment(command.get("environment", {}))
        for path in _array(command.get("depends", []), "command depends", 64):
            _template(path, "dependency path")
        bounded_int(command.get("timeout_seconds", 1200), "timeout_seconds", 1, 86400)
        bounded_int(command.get("max_output_bytes", 1024 * 1024), "max_output_bytes", 1024, 16 * 1024 * 1024)
    return value


def _steps(value: Any, commands: dict) -> list:
    ids = set()
    for row in _array(value, "steps", MAX_STEPS, empty=False):
        object_fields(row, {"id", "command", "outputs"}, {"id", "command"}, "step")
        name = identifier(row["id"], "step id")
        if name in ids:
            raise CoreError("step ids must be unique")
        ids.add(name)
        if row["command"] not in commands:
            raise CoreError("step references an undefined command")
        _outputs(row.get("outputs", []))
    return value


def validate(value: dict) -> dict:
    base = {"schema", "kind", "id", "description", "inputs", "commands", "environment", "outputs"}
    kind = value.get("kind")
    allowed = base | ({"steps"} if kind == "recipe" else {"setup", "actions", "ports", "companions"})
    object_fields(value, allowed, {"schema", "kind", "id", "commands"}, "recipe/workspace")
    if value["schema"] != 1 or type(value["schema"]) is not int or kind not in {"recipe", "workspace"}:
        raise CoreError("expected recipe/workspace schema 1")
    identifier(value["id"])
    if "description" in value:
        text(value["description"], "description", 2000)
    _inputs(value.get("inputs", {}))
    commands = _commands(value["commands"])
    _environment(value.get("environment", {}))
    _outputs(value.get("outputs", []))
    if kind == "recipe":
        _steps(value.get("steps"), commands)
    else:
        actions = _mapping(value.get("actions", {}), "actions")
        if not actions and "setup" not in value:
            raise CoreError("workspace requires setup or actions")
        if "setup" in actions:
            raise CoreError("setup is reserved for workspace setup steps")
        if "setup" in value:
            _steps(value["setup"], commands)
        for action in actions.values():
            object_fields(action, {"steps", "outputs"}, {"steps"}, "workspace action")
            _steps(action["steps"], commands)
            _outputs(action.get("outputs", []))
        for port in _mapping(value.get("ports", {}), "ports").values():
            bounded_int(port, "port", 1, 65535)
        for companion in _mapping(value.get("companions", {}), "companions", 8).values():
            object_fields(companion, {"command", "ready_port", "startup_seconds"}, {"command"}, "companion")
            if companion["command"] not in commands:
                raise CoreError("companion references an undefined command")
            if "ready_port" in companion and companion["ready_port"] not in value.get("ports", {}):
                raise CoreError("companion readiness references an undefined port")
            bounded_int(companion.get("startup_seconds", 10), "startup_seconds", 1, 60)
    return copy.deepcopy(value)


def source(repo: Path, locator: str) -> tuple[dict, dict[str, Path], dict]:
    if locator.startswith("pack:"):
        from .catalog import pack_source
        return pack_source(repo, locator)
    path = Path(locator).expanduser()
    if not path.is_absolute():
        path = under(repo, locator)
    elif path.is_relative_to(repo):
        path = under(repo, path.relative_to(repo).as_posix())
    value = load_definition(path)
    label = path.relative_to(repo).as_posix() if path.is_relative_to(repo) else path.name
    return value, {}, {"label": label, "sha256": digest(value)}


def bundle(repo: Path, locator: str, overlay: str = "") -> dict:
    raw, resources, origin = source(repo, locator)
    base = validate(raw)
    overlay_digest = None
    if overlay:
        if base["kind"] != "recipe":
            raise CoreError("project overlays currently apply to recipes")
        patch, _, _ = source(repo, overlay)
        object_fields(patch, {"schema", "kind", "base_digest", "commands", "steps", "outputs"}, {"schema", "kind", "base_digest", "steps"}, "recipe overlay")
        if patch["schema"] != 1 or type(patch["schema"]) is not int or patch["kind"] != "recipe-overlay" or patch["base_digest"] != origin["sha256"]:
            raise CoreError("overlay does not match the base definition digest")
        additions = _commands(patch.get("commands", {}))
        if additions.keys() & base["commands"].keys():
            raise CoreError("overlay commands must have new names; replace steps explicitly")
        base["commands"].update(additions)
        _steps(patch["steps"], base["commands"])
        replacements = {row["id"]: row for row in patch["steps"]}
        old_ids = {row["id"] for row in base["steps"]}
        base["steps"] = [replacements.get(row["id"], row) for row in base["steps"]]
        base["steps"] += [row for row in patch["steps"] if row["id"] not in old_ids]
        base["outputs"] = base.get("outputs", []) + patch.get("outputs", [])
        base = validate(base)
        overlay_digest = digest(patch)
    return {"definition": base, "resources": resources, "source": origin,
            "definition_digest": origin["sha256"], "overlay_digest": overlay_digest,
            "effective_digest": digest(base)}


def expand(value: str, bindings: dict) -> str:
    def replace(match):
        category, name = match.groups()
        if name not in bindings[category]:
            raise CoreError("unknown or unavailable interpolation: " + match.group(0))
        item = bindings[category][name]
        if isinstance(item, bool):
            return "true" if item else "false"
        return str(item)
    result = TOKEN_RE.sub(replace, value)
    text(result, "expanded value", 16384, empty=True)
    return result


def _repo_path(repo: Path, value: str, *, dot: bool = False) -> Path:
    path = under(repo, value, dot=dot)
    if ".oms" in path.relative_to(repo).parts:
        raise CoreError("recipe cwd, dependencies and outputs cannot target OMS state")
    return path


def _env(entries: dict, bindings: dict) -> dict[str, str]:
    result = {}
    for name, entry in entries.items():
        if "value" in entry:
            result[name] = expand(entry["value"], bindings)
        else:
            ref = entry["from_env"]
            if ref not in os.environ:
                raise CoreError("required environment reference is missing: " + ref)
            result[name] = text(os.environ[ref], "environment reference", 16384, empty=True)
    return result


def base_environment() -> dict[str, str]:
    result = {}
    for name in ("PATH", "HOME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "TERM"):
        if name in os.environ:
            value = os.environ[name]
            if SECRET_VALUE_RE.search(value):
                raise CoreError("inherited environment contains sensitive-looking material")
            result[name] = value
    result.setdefault("PATH", os.defpath)
    result["OMS_RECIPE_PROCESS"] = "1"
    # Preserve, never synthesize or clear, the existing cooperative child boundary.
    for name, value in os.environ.items():
        if name.startswith("OMS_HARNESS_"):
            result[name] = value
    return result


def compile_plan(repo: Path, loaded: dict, inputs: dict, *, action: str = "", ports: dict | None = None, companions: list[str] | None = None) -> dict:
    definition = loaded["definition"]
    inputs = bind_inputs(definition.get("inputs", {}), inputs)
    ports = ports or {}
    companions = companions or []
    if definition["kind"] == "recipe":
        if action or ports or companions:
            raise CoreError("action, ports and companions require a workspace")
        steps = definition["steps"]
        output_specs = definition.get("outputs", [])
    else:
        if action == "setup" and "setup" in definition:
            selected = {"steps": definition["setup"]}
        elif action in definition.get("actions", {}):
            selected = definition["actions"][action]
        else:
            raise CoreError("select an existing workspace action or setup")
        steps = selected["steps"]
        output_specs = definition.get("outputs", []) + selected.get("outputs", [])
        if ports.keys() - definition.get("ports", {}).keys():
            raise CoreError("unknown workspace port")
        if set(companions) - definition.get("companions", {}).keys() or len(set(companions)) != len(companions):
            raise CoreError("unknown or duplicate companion")
    effective_ports = dict(definition.get("ports", {}), **ports)
    for port in effective_ports.values():
        bounded_int(port, "port", 1, 65535)
    if len(set(effective_ports.values())) != len(effective_ports):
        raise CoreError("workspace ports must be distinct")
    bindings = {"input": inputs, "port": effective_ports,
                "resource": {name: str(path) for name, path in loaded["resources"].items()}}
    environment = base_environment()
    environment.update(_env(definition.get("environment", {}), bindings))

    def command(name):
        raw = definition["commands"][name]
        argv = [expand(item, bindings) for item in raw["argv"]]
        cwd = _repo_path(repo, expand(raw.get("cwd", "."), bindings), dot=True)
        if not cwd.is_dir():
            raise CoreError("command cwd is not an existing directory")
        env = dict(environment, **_env(raw.get("environment", {}), bindings))
        if "/" in argv[0]:
            executable = Path(argv[0])
            if not executable.is_absolute():
                # Executable links are normal for virtual environments. Cwd
                # and data paths stay confined, but a declared executable may
                # link to its installed interpreter just like a PATH command.
                executable = cwd / relative(argv[0], "executable path")
            elif executable not in loaded["resources"].values():
                raise CoreError("absolute executables must be selected pack resources")
            executable = str(executable) if executable.is_file() and os.access(executable, os.X_OK) else ""
        else:
            search_path = os.pathsep.join(str(Path(entry) if Path(entry).is_absolute() else cwd / entry) for entry in env["PATH"].split(os.pathsep))
            executable = shutil.which(argv[0], path=search_path) or ""
        if not executable:
            raise CoreError("declared command is unavailable: " + raw["argv"][0])
        dependencies = []
        for item in raw.get("depends", []):
            rel = expand(item, bindings)
            data = read_bytes(_repo_path(repo, rel), 16 * 1024 * 1024)
            dependencies.append({"path": rel, "sha256": sha256_bytes(data)})
        # PATH resolution and declared inputs are pinned before the first process.
        binary = Path(executable).resolve()
        executable_digest = sha256_bytes(read_bytes(binary, 64 * 1024 * 1024))
        invocation = str(Path(executable).absolute())
        return {"name": name, "argv": argv, "executable": invocation, "cwd": cwd.relative_to(repo).as_posix(),
                "environment": env, "environment_digest": digest(env), "dependencies": dependencies,
                "executable_digest": executable_digest, "executable_path_digest": digest(invocation), "command_digest": digest(argv),
                "timeout_seconds": raw.get("timeout_seconds", 1200), "max_output_bytes": raw.get("max_output_bytes", 1024 * 1024)}

    def outputs(specs):
        result = []
        names = set()
        for item in specs:
            row = dict(item, path=expand(item["path"], bindings))
            _repo_path(repo, row["path"])
            if row["path"] in names:
                raise CoreError("duplicate output requirement")
            names.add(row["path"])
            result.append(row)
        return result

    planned = [{"id": step["id"], "command": command(step["command"]), "outputs": outputs(step.get("outputs", []))} for step in steps]
    step_outputs = [item["path"] for step in planned for item in step["outputs"]]
    if len(set(step_outputs)) != len(step_outputs):
        raise CoreError("steps must use distinct output paths for resumable evidence")
    sidecars = []
    for name in companions:
        raw = definition["companions"][name]
        sidecars.append({"id": name, "command": command(raw["command"]), "ready_port": effective_ports.get(raw.get("ready_port")),
                         "startup_seconds": raw.get("startup_seconds", 10)})
    if sum(row["command"]["timeout_seconds"] for row in planned) + sum(row["startup_seconds"] for row in sidecars) > 86400:
        raise CoreError("a recipe invocation's declared time budget cannot exceed 86400 seconds")
    plan = {"schema": 1, "id": definition["id"], "kind": definition["kind"], "action": action,
            "definition_digest": loaded["definition_digest"], "overlay_digest": loaded["overlay_digest"],
            "effective_digest": loaded["effective_digest"], "source": loaded["source"],
            "inputs": inputs, "ports": effective_ports, "steps": planned, "companions": sidecars, "outputs": outputs(output_specs)}
    if len(json.dumps(plan_signature(plan), ensure_ascii=False).encode("utf-8")) > 512 * 1024:
        raise CoreError("expanded recipe plan exceeds 512 KiB")
    plan["plan_digest"] = digest(plan_signature(plan))
    return plan


def plan_signature(plan: dict) -> dict:
    result = {key: plan[key] for key in ("id", "kind", "action", "definition_digest", "overlay_digest", "effective_digest", "source", "inputs", "ports", "outputs")}
    for key in ("steps", "companions"):
        result[key] = []
        for step in plan[key]:
            command = step["command"]
            row = {name: value for name, value in step.items() if name != "command"}
            row["command"] = {name: command[name] for name in ("name", "command_digest", "cwd", "environment_digest", "dependencies", "executable_digest", "executable_path_digest", "timeout_seconds", "max_output_bytes")}
            result[key].append(row)
    return result


def public_plan(plan: dict) -> dict:
    result = copy.deepcopy(plan)
    for row in result["steps"] + result["companions"]:
        command = row["command"]
        command["environment_names"] = sorted(command.pop("environment"))
        command.pop("executable")
    result["execution"] = "explicit --execute required; supervised host process, not a sandbox"
    return result
