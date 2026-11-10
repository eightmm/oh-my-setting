# Recipes, workspace actions, local packs and linked work

`oms recipe` runs a deliberately selected local workflow and records what
actually happened. It also manages workspace actions, local extension resources,
cross-repository feature links, and pending requests from explicit ticks or
submitted events. These are agent-side commands: the operating agent inspects
definitions and handles receipts, recovery and material authority decisions.

All commands return JSON. `--repo PATH` selects the execution directory and its
private recipe state; it can precede the main verb or follow the final action.
This feature does not change task status, grant publication authority, import
leases, or replace the existing delegation/admission/landing workflow.

## Inspect and run a recipe

```bash
oms recipe validate templates/recipes/check.json --repo .
oms recipe plan templates/recipes/check.json --input path=scripts --repo .
oms recipe run templates/recipes/check.json --input path=scripts --execute --repo .
oms recipe show RUN_ID --repo .
oms recipe history --id RUN_ID --limit 20 --repo .
```

`validate` checks the static contract. `plan` resolves inputs, local commands,
declared dependencies and required environment references without starting
processes or creating state. `run` requires `--execute` and executes the exact
planned argv. This switch identifies an explicit invocation; it is not a grant
to commit, push, install software or perform other separately controlled work.

A recipe has a schema version, an id, typed inputs, named commands and ordered
steps. Steps refer to commands instead of carrying shell fragments:

```json
{
  "schema": 1,
  "kind": "recipe",
  "id": "oms-report",
  "inputs": {
    "suite": {"type": "enum", "choices": ["unit", "smoke"], "default": "unit"},
    "repeats": {"type": "integer", "min": 1, "max": 5, "default": 1},
    "output": {"type": "path", "default": "report.json"}
  },
  "commands": {
    "check": {
      "argv": ["python3", "scripts/check.py", "--suite", "${input.suite}",
               "--repeats", "${input.repeats}", "--output", "${input.output}"],
      "depends": ["scripts/check.py"],
      "cwd": ".",
      "timeout_seconds": 300,
      "max_output_bytes": 1048576
    }
  },
  "steps": [
    {"id": "check", "command": "check",
     "outputs": [{"path": "${input.output}", "min_bytes": 1}]}
  ]
}
```

The project owns `scripts/check.py` and its behavior. Commands must already be
available; there is no implicit install, model call, dependency resolution or
network fetch. Executables are either a PATH command, an executable relative to
the selected cwd, or an explicitly selected executable pack resource. Cwd must
already exist and remain inside the selected repository. A setup step may create
directories for a later invocation; planning a single invocation does not assume
that a future step has already created its cwd.

Input types are `string`, `integer` (signed 64-bit range), finite `number`,
`boolean`, normalized relative `path`, and string `enum`. Defaults are validated.
An input without a default is required; `required: false` requires a default.
Numeric inputs accept `min`/`max`; strings accept `max_length` up to 4096 bytes.
Unknown inputs, duplicate CLI assignments, unknown schema fields and unsupported
interpolations fail closed. Boolean CLI values are `true` or `false`.

`${input.NAME}`, `${port.NAME}` and `${resource.NAME}` are the only substitutions.
Replacement is a single deterministic pass. Each resulting string stays in one
argv item: spaces, quotes, semicolons and `$()` in an input are ordinary argument
bytes. OMS does not call a shell to interpret them. The executable cannot come
from an input or port. Interpolation into a known interpreter's `-c`/`-e` code
argument is refused; pass data as additional argv items. Scripts and programs
still have their own argument semantics, so reviewed definitions must use each
program's data/option boundaries correctly.

`depends` pins bounded regular files relative to the repository, such as a local
script or configuration. The command binary, declared dependency digests,
effective environment, arguments, definitions, selected action, ports and
companions contribute to the plan digest. Recipes do not infer dependencies or
hash the whole project. Declare the inputs whose changes must invalidate resume;
start a new run when checks need to execute against new project content.
Executable symlink targets are hashed without replacing the invocation path, so
a selected virtual-environment interpreter keeps its environment identity.

## Outputs, failure and resume

An output requirement describes a regular file inside the selected repository.
It can specify `min_bytes`, `max_bytes`, an expected lowercase `sha256`, and
`fresh` (default `true`). A fresh output must be newly created or have different
content/size/modification time from its pre-execution snapshot. `fresh: false`
allows checking an existing file deliberately. Cwd, dependency and output paths
cannot traverse symlinks, escape the repository or target private OMS state.

Step outputs are checked immediately after a successful command. Different steps
must declare distinct output paths so that completed evidence remains usable.
Top-level `outputs` are final requirements checked against the snapshot taken
before the run. Receipts retain output paths, sizes and SHA-256 values, together
with each command's exit result, duration and dependency/command/environment
digests. Console bytes are bounded, hashed and scanned for sensitive-looking
content; raw stdout/stderr are not retained or replayed into the terminal.
Use explicit output files for useful reports.

```bash
oms recipe resume RUN_ID --file recipes/check.json --execute --retry-failed
oms recipe resume RUN_ID --file recipes/check.json --execute --retry-uncertain
```

Resume re-reads the supplied definition and optional `--overlay`. It rejects
changes to the plan digest, modified completed outputs and incomplete completed
receipts. Successful steps are skipped. A failed step is retried only with
`--retry-failed`; interrupted or incompletely receipted execution requires
`--retry-uncertain`, acknowledging that local side effects may be repeated.
A recorded prior process that still appears live prevents replay. A completed
run is inspected and returned without rerunning its commands.

The run and attempt records are written before process launch. SIGINT/SIGTERM,
timeouts, output overflow, companion exit and failed output requirements produce
failure/interruption receipts. Normal cleanup terminates the invocation's process
groups. A hard-killed supervisor cannot promise cleanup; resume does not infer
that an unobserved operation was harmless. A process can also deliberately escape
its process group. Execution is supervised `trusted-local` host execution, **not
a sandbox**: host network and readable host credential files remain accessible.
These receipts make no stronger isolation claim.

## Project step overlays

An explicit project overlay replaces steps by id and appends new step ids in the
overlay's order. It can add named commands and final output requirements. It
cannot silently replace an existing command name or remove steps. The reviewed
base's canonical JSON digest is mandatory:

```json
{
  "schema": 1,
  "kind": "recipe-overlay",
  "base_digest": "SHA256_FROM_RECIPE_VALIDATE",
  "commands": {"project-check": {"argv": ["python3", "-m", "unittest"]}},
  "steps": [{"id": "check", "command": "project-check"}]
}
```

```bash
oms recipe plan recipes/check.json --overlay recipes/project.json
oms recipe run recipes/check.json --overlay recipes/project.json --execute
```

Changing the base invalidates the overlay binding. Both definition digests are
retained in the run receipt. Workspace actions currently use their own complete
definition; project overlays apply to recipes.

## Workspace setup, actions and companions

The example [workspace.json](../templates/recipes/workspace.json) defines setup,
an inspection action, a nonsecret environment value and an optional local HTTP
companion. From the OMS source checkout:

```bash
oms recipe workspace plan templates/recipes/workspace.json setup
oms recipe workspace run templates/recipes/workspace.json setup --execute
oms recipe workspace plan templates/recipes/workspace.json inspect --port http=43124 --with http
oms recipe workspace run templates/recipes/workspace.json inspect --port http=43124 --with http --execute
```

A workspace uses the same `inputs`, `commands`, `environment` and `outputs`
contracts as recipes. `setup` is an explicit ordered step list. `actions` maps
names to objects with `steps` and optional `outputs`; `setup` is reserved.
`ports` maps names to defaults in 1..65535, with explicit `--port NAME=NUMBER`
overrides and distinct effective values. No ports are silently allocated,
forwarded, opened in a browser or published.

Environment entries have exactly one of `{"value":"literal or interpolation"}`
and `{"from_env":"EXISTING_VARIABLE"}`. Missing references fail before execution.
The process inherits a small ordinary environment (`PATH`, `HOME`, temporary
directory, locale and terminal variables) and existing harness child markers.
Secret-looking names/values are refused. The contract cannot override PATH,
HOME, interpreter/loader injection variables or `OMS_*` authority controls.
Reference values are not stored in receipts, only their effective digest.
Environment filtering does not restrict access to host files.

`companions` maps names to an existing command, optional `ready_port` referring
to a configured port, and `startup_seconds` (1..60). Only names selected with
`--with` launch. An already occupied readiness port is refused. Readiness means
that a local TCP connection succeeded while the companion process remained alive;
it is not an application health or process-ownership attestation. A companion
exiting during a main command fails the invocation. Companions are stopped after
completion/failure and restarted only by a new explicit execution/resume. They
do not become background daemons. Companion output is discarded.

## Local extension packs

A pack is a local directory with `manifest.json` and explicitly named resources.
Resources may be `recipe`, `script` or `renderer`; each has a normalized relative
path, a SHA-256 of its exact bytes and optional `executable: true`. A recipe
resource must be a valid recipe/workspace definition. Pack ids start with `oms-`.

```json
{
  "schema": 1,
  "kind": "local-pack",
  "id": "oms-project-tools",
  "version": "1.0.0",
  "resources": {
    "report": {"kind": "recipe", "path": "recipes/report.json", "sha256": "EXACT_FILE_SHA256"},
    "helper": {"kind": "script", "path": "scripts/report.py", "sha256": "EXACT_FILE_SHA256"},
    "renderer": {"kind": "renderer", "path": "renderers/html.py", "sha256": "EXACT_FILE_SHA256"}
  }
}
```

```bash
oms recipe pack inspect ./local-tools
oms recipe pack import ./local-tools --digest MANIFEST_DIGEST --resource report --resource helper
oms recipe pack import ./local-tools --digest MANIFEST_DIGEST --resource renderer
oms recipe pack list
oms recipe plan pack:oms-project-tools@1.0.0/report
oms recipe run pack:oms-project-tools@1.0.0/report --execute
```

Inspection verifies all resource bytes and returns the canonical manifest digest.
Import binds that digest and copies only explicitly selected resources; omitting
`--resource` selects all. Later imports of the same immutable version may add
selections. Reusing an id/version with different manifest bytes is refused.
The selection receipt is published after the resource writes. Missing,
unselected, changed, symlinked or escaping resources fail closed. Every selected
resource is reverified when a packed recipe is loaded.

A packed recipe can use `"argv": ["python3", "${resource.helper}"]` or call a
selected executable resource directly. Renderers are ordinary local resources
used by explicit recipe commands; import does not render content or execute
scripts. There is no package registry, installation hook, dependency installation,
provider plugin activation or hidden download. Sensitive-looking resource content
is refused. Packs are versioned distribution bytes, not a sandbox or authority
grant.

## Cross-repository feature worksets

Worksets link one shared specification to named repositories and opaque task
references. See [workset.json](../templates/recipes/workset.json).

```bash
oms recipe workset put templates/recipes/workset.json
oms recipe workset show oms-shared-feature
oms recipe workset list
oms recipe workset put templates/recipes/workset.json --replace
```

Repository paths are relative to the selected coordinator and may explicitly use
`../sibling`. The shared spec names one of those repos and a bounded regular file
inside it. `put` pins the spec digest; `show` reports current/changed/missing spec
content, repository availability, current Git HEAD when available, and the given
task references. Changing an existing workset requires `--replace`.

Inspection does not clone/fetch repositories, parse another agent's task schema,
copy private state, acquire leases, execute remote commands, modify any linked
repository or authorize work there. A missing secondary repository can remain a
visible link; the shared spec must be present when the workset is registered.

## Explicit schedules and events

Triggers are immutable request definitions, pinned to recipe/overlay digests.
They select a recipe or workspace action plus fixed inputs, optional ports and
companions. Exactly one condition is allowed: a UTC `schedule` with `at` and
optional positive `every_seconds`, or an `event` with a name and optional flat
scalar equality `match` filters. Event values are data; they are never substituted
into commands or promoted into input values.

```bash
oms recipe trigger add templates/recipes/scheduled-check.json
oms recipe trigger tick
oms recipe trigger add templates/recipes/event-check.json
oms recipe trigger event source-changed --event-id change-42 --data '{"branch":"main"}'
oms recipe trigger requests
oms recipe trigger plan REQUEST_ID
oms recipe trigger run REQUEST_ID --execute
oms recipe trigger cancel REQUEST_ID
oms recipe trigger disable oms-scheduled-check
oms recipe trigger enable oms-scheduled-check
oms recipe trigger list
```

Ticks and event submission **only create pending requests**. There is no daemon,
cron installation, automatic CI repair, automatic merge, new main agent or
implicit execution. `tick --now YYYY-MM-DDTHH:MM:SSZ` provides an explicit clock
for deterministic checks. Repeated ticks at the same schedule occurrence do not
duplicate requests. Overdue recurring occurrences coalesce into one latest
request with a count of skipped occurrences; enabling an overdue trigger uses
the same policy.

Event ids are idempotent per event name. Reusing an id with different payload
bytes is refused. Flat filters use exact types and equality; a boolean does not
equal an integer. Payload text is not stored in event/history records: only its
digest, selected trigger ids and request ids remain. Configured filter values
remain part of the explicit trigger definition.

`trigger run` is the separate explicit execution step. It rechecks pinned
definition digests, reserves one run id, and uses the ordinary recipe receipts.
Repeating a successful invocation returns that run without replay. Failure and
uncertain recovery use `--retry-failed`/`--retry-uncertain` on the same request.
Only pending requests may be cancelled. To change a trigger's definition, disable
its old id and register a new id; existing requests retain their original binding.

## State, limits and verification

Typed writers own private recipe runs, append-only history, imported packs,
worksets, triggers, events and requests. Writes use the existing runtime's atomic
writer and shared lock protocol. Neither saved definitions nor request records
carry execution, approval, lease, merge or publication authority. Marked harness
children and recipe subprocesses may use only the semantically classified
read-only recipe actions; they cannot recursively execute or mutate this state.

Current bounds are 256 KiB per definition; 64 inputs/commands/steps/resources;
128 argv items; eight companions; 4 MiB per pack resource and 16 MiB per selected
pack; 16 MiB default/64 MiB maximum per output file; 256 attempts per step or
companion history; and 1024 records per flat state collection. Command timeouts
are 1..86400 seconds, with an aggregate declared invocation budget of at most one
day. Console limits are 1 KiB..16 MiB. Capacity errors fail closed; this first
scope does not silently prune receipts or implement automatic archival.
Expanded plan signatures are capped at 512 KiB and receipt growth stops before
the runtime's 8 MiB state-file ceiling; collection scans are capped at 32 MiB.

`tests/test_recipes.py` covers this new feature family using isolated temporary
repositories, homes and lock directories with harmless local Python commands.
Coverage includes real execution/resume/failure/interruption, argv injection
resistance, output freshness/drift, changed definitions/dependencies, overlays,
environment filtering, local companion cleanup, selective packs and renderers,
immutable versions, workset spec drift, due/event idempotency and child guards.

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p test_recipes.py -v
bash -n scripts/recipe.sh
```
