# Installing oh-my-setting

Host requirements, profiles and maintenance for Linux, macOS and Windows via WSL2.
For the one-line install and a summary of changed files, see the [README](../README.md).
[한국어](INSTALL.ko.md)

Register another account with `oms connect-services --account codex:ID`,
`--account claude:ID`, or `--account pi:ID`; see
[provider accounts](TERMINAL-PANEL.md#provider-accounts) for private homes
(`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`), routing, doctor
checks, repair and removal. Manual config-scoped claude.ai use is documented;
OMS isolation `--verify` attestation remains unsupported/uncertain because
native selection fields do not prove credential origin. Native status may
initialize metadata in its selected root; registration, doctor and update never
run status. Claude state stays separate from legacy Codex and Pi files, and
old versions cannot manage them or protect against arbitrary home collisions.
Antigravity OAuth account isolation remains unproved and fails closed
(`provider account isolation is unproved`).

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## Hosts

| Host | Needs | Managed files |
|---|---|---|
| Linux (glibc) | Bash 5.0+, curl; Git, tar, gzip and find are installed if missing (root or sudo needed then) | symlinks |
| macOS | Bash 5.0+, curl, Command Line Tools (`xcode-select --install`; without them `git` is only an installer stub). The installer can re-exec under an installed Homebrew Bash 5; otherwise install Bash with `brew install bash` and rerun it. | symlinks |
| Windows via WSL2 | Linux requirements above, inside WSL2 | symlinks |

Claude Code's [memory documentation](https://code.claude.com/docs/en/memory.md) says a
Cowork session on the desktop skips a `~/.claude/CLAUDE.md` that is itself a symlink or
hard link. Under the default link mode the global rules are such a symlink, so they may not
load on that surface; the terminal CLI is unaffected. `OH_MY_SETTING_LINK_MODE=copy` installs
regular files that `oms doctor` checks for parity instead. OMS has not tested the Cowork surface.

OMS lifecycle commands run as native Rust binaries; installation and scheduled
updates do not provision or require a private Python interpreter. A cold install
or update builds the selected checkout with Cargo and the toolchain declared by
that checkout's `rust-toolchain.toml`. Native and shell builds remove an inherited
`RUSTUP_TOOLCHAIN` from the Cargo child only; the caller's environment is preserved.
Install Rust through rustup before using a source checkout without a verified
staged payload. The build keeps `Cargo.lock`
with `--locked` and can fetch missing dependencies. Set `CARGO_NET_OFFLINE=true`
only when the required toolchain and dependencies are already cached.

Managed tool artifacts support glibc Linux and macOS on amd64/arm64. musl-based
Linux such as Alpine is unsupported by this artifact selection.

On Ubuntu 24.04+ with AppArmor unprivileged user namespaces restricted,
`oms doctor` warns that Codex sandboxed runs may fail; allow user namespaces
for `bwrap` in an AppArmor profile or set
`kernel.apparmor_restrict_unprivileged_userns=0` with sysctl.

## Windows via WSL2

Use **WSL2**, not WSL1. Keep both the repository and `~/.oh-my-setting`
on the Linux filesystem, along with your WSL home; do not use `/mnt/c`.
Install and run provider CLIs inside WSL, using the Linux requirements above.
Native Windows (Git Bash/MSYS2/Cygwin) is unsupported and OMS refuses to run;
`scripts/install-tools.sh` and `scripts/doctor.sh` also refuse it when run
directly; the managed artifact catalog has no native Windows target.

<details>
<summary>Remove a legacy native Windows installation</summary>

To uninstall an old native Windows installation, use the last Windows-supporting
release, `oms-windows-final`, from the existing installation checkout:

```bash
cd ~/.oh-my-setting
git fetch origin tag oms-windows-final
git switch --detach oms-windows-final
bash scripts/uninstall.sh
```

Preserve any local checkout changes before switching. The current release also
refuses `oms uninstall` on native Windows. An already-running older updater
also stops when it enters the new tree and rolls back the checkout and managed
links; start the WSL2 installation separately.

</details>

## Installation details

### Profiles and integrations

The default installer selects the `core` capability: the harness, Bash, Git,
the native OMS runtime, and one coding-agent provider. It does not make GitHub CLI, Notion CLI,
all three providers, research tooling, or cluster tools mandatory. Use the
`full` compatibility profile only on machines that should carry the historical
all-provider/GitHub/Notion/research footprint.

Capability profiles are `core`, `council`, `github`, `notion`, `research`,
`hpc`, `container`, `remote`, and `full`. The selective installer reuses the
existing verified download transactions, records the exact requested profile in a
private receipt, and reapplies only that tool set during updates. Existing
installs without a capability receipt retain the legacy full-tool update path
until an agent explicitly migrates them. See
[MIGRATION.md](MIGRATION.md#capability-profiles).

| `oms install-profile` options | Effect |
|---|---|
| `--apply --add --profile github` | Add a capability; retain profiles, provider and degraded policy. |
| `--profile NAME` without `--add` | Replace the recorded selection. |
| `--reapply` | Reuse the recorded selection exactly. |
| `--plan` | Preview either operation without changes. |

An ordinary reinstall preserves the saved profiles, primary provider and
allow-missing policy. Bare `install-tools`, `--only` and `--providers` perform
tool operations without replacing that selection. Explicit profile/full/add/reapply
operations record it. `--receipt` overrides `OMS_CAPABILITY_RECEIPT`, then the
default OMS configuration path; relative paths use the invocation directory.
Updates forward the selected receipt to the incoming process before changing its
working directory. During updates, an explicitly selected missing or unsafe
receipt is an error; it never falls back to refreshing every tool.

`OH_MY_SETTING_MCP=0` and `OH_MY_SETTING_AGY_PLUGIN=0` skip their respective
integrations. Installation records both choices in the receipt; environment-clean
updates and scheduled triggers preserve them. An explicit environment override
on `oms update` changes the recorded choice. Antigravity also accepts `auto`
(the default: install only when available) or `1` (require it).

### Tool installation and updates

Only installing a missing Git, tar, gzip or find needs root. OMS keeps managed
tool identities in its native installer and resolves release versions from the
official upstream source when a tool is missing or an upgrade is requested.
There is no checked-in default version lock. An ordinary install preserves tools
already present; `oms update` refreshes the selected tool set unless `--no-tools`
is requested.

| Tool | Default release source | Payload check |
|---|---|---|
| Node | Newest LTS in the official Node distribution index | Versioned archive and `SHASUMS256.txt` SHA-256 |
| uv | Latest non-prerelease GitHub release from `astral-sh/uv` | Exact current-host asset and its SHA-256 release digest |
| gh | Latest non-prerelease GitHub release from `cli/cli` | Exact current-host asset and its SHA-256 release digest |
| Claude, Codex, ntn | The package's npm `latest` dist-tag | Package identity, exact version and SHA-512 SRI; Claude/Codex native package identity and SRI too |
| Antigravity | Its official current-host updater manifest | Versioned artifact URL and SHA-512 |

Node follows LTS rather than a prerelease or the highest Current major. Only the
current host's payload must be available; a delayed release on another platform
does not block this host. A failed or malformed metadata response is an error,
not permission to install an old saved release. Default upgrades preserve an
already installed version newer than the resolved release.

The private `tool-releases.json` receipt under the OMS data directory records
resolved versions, URLs or package coordinates, integrity fields and observation
times. It is an observation record, not a policy for the next update or proof
that every recorded version is installed. Older `provider-tools.lock.json`
provider observations can be carried forward without choosing future versions.
`oms doctor --tool-lock` checks either these records or an explicit pin; it does
not contact upstream or prove installed-byte integrity. Fingerprints exclude
observation timestamps.

To opt into exact versions, set `OH_MY_SETTING_TOOL_LOCK` to a schema-2 JSON
file containing the selected tools and current-host payload metadata. Relative
paths are resolved from the command's working directory. An explicit pin is
validated and may deliberately select an older version. Clearing the override
restores normal upstream resolution; no repository edit is needed.

Recognized vendor-native Claude, Codex and Antigravity installations retain their
vendor update commands. OMS checks successful version probes and rejects a
reported version decrease; these updates do not claim that vendor-managed bytes
match a previous OMS observation. Fresh managed Claude/Codex payloads are
verified through npm package metadata before publication. `ntn` may run its
verified package's installation script. Explicit pins use the verified exact
payload path instead of the default vendor update command.

When a bootstrapped Antigravity binary hands over to its vendor updater, OMS
validates its ownership checksum sidecar before probing the binary's version.
An earlier vendor update may have left a valid OMS sidecar with an outdated
checksum. This case is allowed only on the recognized vendor path for a safe
sidecar owned by the current user. OMS checks that the sidecar and binary remain
unchanged through the version probe. If no update runs, the sidecar remains in
place. Immediately before handing off to the updater, OMS retires the sidecar
and warns if its checksum was stale. Malformed, unsafe, or changed sidecars
are refused. The vendor's binary and this handoff are outside OMS rollback.
An explicit pin still requires a matching ownership checksum before replacement.

Already installed Pi is refreshed with `pi update self` only through
`oms install-tools --providers`. Other recognized optional npm providers in a
writable user-owned global prefix use their current `latest` dist-tag; there is
no 14-day release delay or npm `--before` restriction. npm lifecycle scripts may
execute package code. An explicit tool lock skips these optional refreshes
because its schema does not describe them.

Provider dependencies are selected with the provider: Claude, Codex and ntn
include Node, installed first even when `--only` lists it last; Antigravity does
not. Verified package downloads use npm 7.18.0 or later for scratch-directory
packing. The default managed Codex and ntn launchers use Node 16 or later. A
compatible npm in a separate prefix is reused. Missing or incompatible tooling
triggers preparation of the verified Node bundle, retaining a suitable installed
runtime when only npm needs repair. If the selected version already has an incomplete
or damaged NVM directory, OMS refuses rather than replacing files whose ownership
it cannot establish; automatic repair of that directory is not implemented.
Downloads and package identities are checked
before managed publication, and local install/update/repair/uninstall changes
share the reversible lifecycle transaction. Vendor update commands manage their
own recovery; their binaries are not part of the OMS checkout rollback.

## Updates

Fresh installations track `edge`, which follows the origin repository's default
branch. An explicit `OH_MY_SETTING_REF`, `--ref`, or saved tracking choice is
preserved. A successful `--fallback-to-edge` also saves `edge` for the next
update. Applying the runtime edge channel keeps tracking its configured branch;
applying a reviewed stable release keeps that exact commit. If no channel
manifest exists, stable is unconfigured until explicitly promoted.

The native updater prepares and checks the incoming payload before publishing
it, using the incoming checkout's Rust toolchain. Missing locked dependencies
can be fetched; offline operation is an explicit caller choice. Automatic
updates preserve local checkout edits and report a blocked/skipped update.
Keep private policy in user skills outside the tracked install tree.

Incoming installation reconciles OMS components without reinstalling tools or
rewriting capability choices. The owning update refreshes the selected tools
after the core commit. `--no-tools` and rollback skip that refresh entirely.
The incoming revision supplies its own installer: rolling back to an older
revision that predates this separation retains that older install behavior.

Generated cron and systemd jobs retain the installation's HOME, XDG locations,
receipt paths and Rust homes, with a PATH that includes the managed bin and Cargo
directories. They do not depend on an interactive shell profile. A temporary
`RUSTUP_TOOLCHAIN` override is not saved, so the incoming checkout can select its
own toolchain. Existing check-only schedules remain check-only.
Explicit tool-lock and provider configuration paths are retained as well.

Systemd installs report whether logout persistence is available; explicitly set
`OH_MY_SETTING_AUTO_UPDATE_LINGER=1` when installing the trigger to enable it
(host permission may be required), or use cron. Linger applies to all this
account's user services, so OMS does not silently enable it by default.

The daily updater applies clean fast-forwards by default and skips dirty or
diverged checkouts.

### Previewing an install or update

`install.sh --plan` and `oms update --plan` print what the run would change and
change nothing: no clone or fetch, no lifecycle lock, no link, backup move,
settings edit or registration. Each line reads `STEP: STATUS: detail`, with
status `change`, `current`, `skip`, `not-previewed` or `note`:

```text
links: change: move your existing ~/.codex/AGENTS.md to ~/.codex/AGENTS.md.backup.<timestamp>; it would no longer be in effect
links: change: link ~/.local/bin/oms -> ~/.oh-my-setting/scripts/oms
claude-hooks: change: register SessionStart hook resume-hook.sh
mcp: not-previewed: install-mcp.sh registers the harness MCP server through the claude and codex CLIs
plan: 38 change(s), 0 already current, 6 step part(s) not previewed; nothing was written
```

A step is previewed by its own code where that code has a write-free mode
(links and backups, the capability profile, Claude hook rows, Antigravity
permission rules, the auto-update trigger). Everything else is named
`not-previewed` rather than described from a second copy of its logic: the
clone or fetch, the native payload build, tool installers and any PATH line they add,
service logins, the Codex plugin and its `config.toml` block, MCP and
Antigravity plugin registration, and the Claude `statusLine`, `modelSettings`
and `attribution` keys. The plan reads the checkout as it is on disk, so an
update plan describes relinking the current commit, not the one a fetch would
bring; with no checkout yet (the curl form before a first install) it says so
and stops.

An existing checkout needs a verified native payload to produce the detailed
install plan. If that payload has not been prepared, `install.sh --plan`
reports it unavailable without building binaries or downloading dependencies.

### Verification policy

The entry points intentionally accept different evidence. A successful workflow
does not by itself mean that the full test selection ran.

| Entry point | Accepted evidence and explicit exception |
|---|---|
| Fresh installation | Installs the selected ref, defaulting to `edge` (the remote default branch), then runs the selected installation checks. CI success is not a prerequisite. |
| Manual `oms update` | Applies the selected ref through the update transaction and runs link/doctor checks unless explicitly skipped. This is the same manual edge exception; it does not query or claim CI success. |
| Scheduled auto-update | Requires a successful `test.yml` push run for the exact selected SHA. Both full and affected selections are accepted. If scope cannot be read, the confirmed workflow success remains sufficient and the state says `scope=unknown; ...; workflow success only`. |
| `oms land` installation | Requires CI success for the pushed SHA plus full proof. Default: a CI run for exactly that SHA on the target whose jobs executed the full selection, either the push run or a dispatched `test.yml` run (`gh workflow run test.yml --ref TARGET`; `oms land --ci-full` dispatches it once, otherwise land checks for an existing run). Option: a full local gate. A pending run or requested receipt handoff blocks until proof is complete. Affected-only or unreadable CI scope, a run for another SHA, and a cancelled or failed run supply no full proof. Skipped or failed CI never authorizes installation. |

Auto-update and land use the same exact-SHA CI query and scope classifier.
Auto-update's existing `message` records the selected SHA, CI success, and
`scope=full`, `scope=affected`, or `scope=unknown`; land records its proof in the
existing `ci.scope` and `update.authority` fields. Neither a missing query result
nor an unknown scope is labeled full verification. Fresh and manual edge installs
remain deliberate exceptions; the installer default and receipt schemas are unchanged.

The core capability profile does not require GitHub login or install `gh`.
CI queries prefer `gh`; when it cannot start or authenticate, the public
`eightmm/oh-my-setting` repository can use an unauthenticated, read-only GitHub
API fallback for `test.yml`. Other repositories, private remotes, enterprise
hosts, and credential-bearing URLs require a working `gh` configuration.
Fallback requests use the fixed HTTPS API host, reject redirects, and are bounded
to 30 seconds per query attempt and 1 MiB per response. Scope requires the exact
run attempt and a complete job list of at most 100 jobs; incomplete evidence is
reported as unknown, never full.

Pending, failed, cancelled, missing, and unavailable CI evidence remain distinct.
If the target has no confirmed success, auto-update searches at most 20 commits
on its first-parent history for a successful descendant of the installed commit.
Without one it records a normal skip, with an unavailable query distinguished
from a failed workflow. Rate limits and network errors do not authorize an update.

Updates run the incoming installation's doctor before committing the core
receipt, unless `--no-doctor` is selected. A failed doctor check keeps the core
change within the rollback transaction. Selected provider tools are refreshed
after that commit, with the tool-specific version and capability checks. There
is no second general integration audit after this refresh. A refresh failure
reports that the core update succeeded; provider binaries remain outside the
core rollback transaction.

Upgrade an existing install with `oms update`; see the
[migration notes](MIGRATION.md#07) for release changes.

## Services and display

When GitHub or Notion capabilities are selected, an interactive installer can
delegate browser login to `gh auth login` and `ntn login` and discover the Work
Journal's Notion target; a non-interactive install records the missing
capability instead of weakening the core runtime.
Claude Code gets compact main and subagent HUDs (model/effort, context,
rate-limit countdowns, cost, Git state) and a `high` effort default for Opus
5.5 unless you set one; Codex gets the equivalent native footer when it has no
user footer.

## Uninstall

Uninstall restores managed configuration but leaves external CLIs and the
user-local PATH entry in place.

The install manages the three global agent rule files — `~/.claude/CLAUDE.md`,
`~/.codex/AGENTS.md`, and `~/.gemini/AGENTS.md`. A pre-existing file is moved
to `<file>.backup.<timestamp>` (announced at install time and reported by
`oms doctor`), stops applying while the install is active, and is restored by
`oms uninstall`.

For Antigravity headless permissions and Notion data-source selection, see
[Components](COMPONENTS.md) and [Work Journal](WORK-JOURNAL.md).

Scripts live in `~/.oh-my-setting/scripts/` and are reachable as `oms <tool>`.
They are documented for transparency and recovery, not for manual use.
