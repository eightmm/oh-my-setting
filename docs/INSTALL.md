# Installing oh-my-setting

Host requirements, profiles and maintenance for Linux, macOS and Windows via WSL2.
For the one-line install and a summary of changed files, see the [README](../README.md).
[한국어](INSTALL.ko.md)

Register another account with `oms connect-services --account codex:ID` or
`--account claude:ID`; see [provider accounts](TERMINAL-PANEL.md#provider-accounts)
for private homes, explicit verification, repair and removal. Codex routing is
unchanged. Manual config-scoped claude.ai use is documented; OMS isolation
verification remains unsupported/uncertain because native selection fields do not
attest credential origin. Native status may initialize metadata in its selected root. Registration, doctor
and update never run status. Claude state is separate from legacy Codex files;
old versions cannot manage it or protect against arbitrary home collisions.
Antigravity OAuth account isolation remains unknown and unavailable.

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

OMS requires Python 3.11+ with no upper version bound and provisions the 3.12
runtime pinned in `tools.lock.json`. System Python stays unchanged, including
older versions on Ubuntu 20.04/22.04 and macOS Command Line Tools.

When system `python3` is missing or older than 3.11, the installer fetches the pinned uv
(sha256-checked), builds its own Python from `tools.lock.json`, and points a
managed `python3` shim at it. musl-based Linux such as Alpine is not
supported: the pinned builds need glibc.

Hook launchers select the managed interpreter even when provider apps inherit
only `/usr/bin:/bin`. Antigravity's MCP registration stores the resolved
interpreter's absolute path. Cleanup and lifecycle help remain available with
an older system Python when no managed runtime exists.

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
directly, and `tools.lock.json` pins no Windows payloads.

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
Python, and one coding-agent provider. It does not make GitHub CLI, Notion CLI,
all three providers, research tooling, or cluster tools mandatory. Use the
`full` compatibility profile only on machines that should carry the historical
all-provider/GitHub/Notion/research footprint.

Capability profiles are `core`, `council`, `github`, `notion`, `research`,
`hpc`, `container`, `remote`, and `full`. The selective installer reuses the
existing locked download transactions, records the exact requested profile in a
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

`OH_MY_SETTING_MCP=0` and `OH_MY_SETTING_AGY_PLUGIN=0` skip their respective
integrations. Installation records both choices in the receipt; environment-clean
updates and scheduled triggers preserve them. An explicit environment override
on `oms update` changes the recorded choice. Antigravity also accepts `auto`
(the default: install only when available) or `1` (require it).

### Provider installation and updates

Only installing a missing Git, tar, gzip or find needs root. Managed bootstrap
versions, platform URLs and integrity values are pinned in `tools.lock.json`.
Fresh provider installs resolve a stable release into the private
`provider-tools.lock.json` snapshot. Locked payloads cover Linux and macOS on
amd64/arm64; rows for the retired Windows targets in an older snapshot or
explicit lock are ignored. Fresh Claude downloads its
native binary and verifies SHA-256 from the vendor HTTPS manifest before
running `install VERSION`; OMS does not verify the manifest’s GPG signature.
Fresh Codex and agy keep their existing verified installation paths. The
snapshot records resolved versions, not a permanent version policy or proof
of installation.

For recognized vendor layouts:

| Provider | Normal refresh | Explicit tool lock |
|---|---|---|
| Claude | `claude update` | `claude install VERSION`; require equality. |
| Codex (native) | `codex update` | Refuse a mismatch before updating; matching pins stay unchanged. |
| agy (native) | `agy update`; vendor-managed on the default channel | Refuse pin mismatch or drift in OMS-marked bytes; matching pins stay unchanged. |
| Pi | `pi update self` only through `install-tools.sh --providers` | Skip refresh: the lock schema has no Pi row. |

> Pi is refreshed only when already installed. Full-tool installs and profile
> reapply leave it unchanged. Manual `--no-tools` skips tool refresh.

<details>
<summary>Detailed refresh rules: integrity, interrupted transactions, Claude migration and inventory</summary>

Recognized Claude, Codex and agy vendor layouts use `claude update`,
`codex update` and `agy update` through `install-tools.sh --providers`, profile
reapply and normal updates. Pi refresh runs only through `install-tools.sh --providers`
using `pi update self`; full-tool installs and profile reapply leave Pi unchanged. OMS checks
that the command still resolves into its vendor layout, `--version` succeeds,
and its version has not decreased; it does not require snapshot equality or
claim to authenticate vendor-updated bytes. Pi is refreshed only when already
installed; OMS does not run its interactive fresh installer. Scheduled apply
refreshes installed providers even when OMS itself is unchanged. Optional
npm-installed providers use stable releases at least 14 days old; their npm
lifecycle scripts can execute package code, and git/URL dependencies are not
subject to that age limit. Manual `--no-tools` skips tool refresh.



#### Explicit pins and integrity

An explicit `OH_MY_SETTING_TOOL_LOCK` pins Claude with `claude install VERSION` and
requires equality. Native Codex and agy refuse a mismatched pin before updating;
matching pins leave them unchanged. Pi has no row in the current lock schema, so an
explicit lock skips Pi refresh with a note and leaves it unchanged. On the default channel agy is vendor-managed after a lock-verified bootstrap,
like Claude and Codex. A fresh install leaves no ownership digest. Refresh retires
an existing, well-formed legacy sidecar only while `~/.local` and `~/.local/bin`
are real directories; changed bytes produce a warning before vendor refresh.
Malformed or linked markers, digest-read errors and failed sidecar removal still
refuse the operation.

Under an explicit lock, an existing OMS ownership digest stays binding: doctor
checks it and refresh refuses changed owned bytes before running the binary.
Restore the verified binary and marker, or preserve both elsewhere and reapply
the profile to install a verified payload. An unmarked vendor installation stays
vendor-owned; a matching pin leaves it unchanged without creating a marker.

An interrupted OMS direct-install transaction with recovery residue
first recovers from a checksum-verified locked payload before vendor refresh. Under an explicit lock, unexpected bytes in an OMS-marked AGY binary remain
refused without transaction recovery evidence. Doctor verifies marked commands
before probes. On the default channel it warns once about a verifiable stale AGY
sidecar and permits the selected tool's probe; malformed or otherwise invalid
ownership evidence still skips execution.

Warning-only mode continues local state checks while skipping unsafe
tool/model/plugin/skill probes; required-tool and integration audits stop.



#### Claude launcher migration

An absent, empty or invalid native versions directory leaves Claude on its existing
npm/shim path. Migration requires an executable numeric native release inside the
resolved vendor versions root. When an OMS-owned Claude npm shim shadows existing native
versions, OMS invokes the newest native binary's installer to restore the vendor
launcher. The proven OMS shim moves to `.claude.oh-my-setting-migration` beside the
launcher while the vendor installs; failure restores it when the launcher is absent, and
a later lifecycle call verifies the native launcher directly, regardless of PATH order,
and finishes cleanup without installing again.

The backup stays until npm cleanup succeeds. Only the npm package in the prefix captured
by that shim is uninstalled, with lifecycle scripts disabled. An npm-owned Claude
launcher under the active npm root remains valid, including the `~/.local` prefix.
Unproven ownership refuses migration and names recovery; a vendor launcher is never
replaced by an OMS shim. A kill after the vendor install but before npm cleanup can
leave an extra npm copy.

#### Provider inventory and cleanup

`oms doctor` lists provider copies across PATH, vendor roots, and every nvm Node prefix,
marks the selected copy, channel and version, and gives prefix-specific npm uninstall
and foreign launcher removal commands. Only the command selected on PATH may execute a
version probe. All extra copies say "not executed", even with a matching sidecar; npm
versions come from package metadata. The `digest` status is `matched`, `unmarked`,
`invalid` or `unknown`. A matching sidecar describes bytes and grants no execution
authority. Vendor-retained release files count as one installation and never receive
cleanup guidance, including when a release directory is on PATH.

Aliases do not add version probes. Cleanup guidance excludes the selected copy and its
aliases. Entries with line or field separators are escaped and reported as unknown
without execution. When an integrity failure disables probes, tool versions are labeled
"not probed". Doctor checks direct-tool transaction residue before accepting a
vendor-channel version. Uninstall and repair leave vendor launchers and vendor roots in
place.

</details>

Provider dependencies are selected with the provider: Codex and Claude include
locked Node, while Antigravity does not. New downloads are verified before use.
Existing external CLIs at the exact version are reused and labeled as
version-only by doctor. Install, update, repair, and uninstall share one
user-wide lifecycle lock.

## Updates

Installation and scheduled updates use a private uv-managed Python pinned in
`tools.lock.json`; system/project Python is only a bootstrap, not the timer's
interpreter. Automatic updates preserve local checkout edits and report them as
`skipped`. Keep private policy in user skills outside the tracked install tree.
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
clone or fetch, the Python runtime, tool installers and any PATH line they add,
service logins, the Codex plugin and its `config.toml` block, MCP and
Antigravity plugin registration, and the Claude `statusLine`, `modelSettings`
and `attribution` keys. The plan reads the checkout as it is on disk, so an
update plan describes relinking the current commit, not the one a fetch would
bring; with no checkout yet (the curl form before a first install) it says so
and stops.

### Verification policy

The entry points intentionally accept different evidence. A successful workflow
does not by itself mean that the full test selection ran.

| Entry point | Accepted evidence and explicit exception |
|---|---|
| Fresh installation | Installs the selected ref, defaulting to `edge` (the remote default branch), then runs the selected installation checks. CI success is not a prerequisite. |
| Manual `oms update` | Applies the selected ref through the update transaction and runs link/doctor checks unless explicitly skipped. This is the same manual edge exception; it does not query or claim CI success. |
| Scheduled auto-update | Requires a successful `test.yml` push run for the exact selected SHA. Both full and affected selections are accepted. If scope cannot be read, the confirmed workflow success remains sufficient and the state says `scope=unknown; ...; workflow success only`. |
| `oms land` installation | Requires CI success for the pushed SHA plus a full local gate or a CI run proving the full selection. Affected-only or unreadable CI scope supplies no full proof; a full local gate can still supply it. Skipped or failed CI never authorizes installation. |

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

After refreshing provider tools, updates run a bounded integration audit of
Claude hooks, the Codex plugin, and enabled MCP/Antigravity interfaces. A failed
audit returns failure and names the successful core update separately; provider
binaries are outside the core rollback transaction. `--no-doctor` (or the
scheduled doctor opt-out) explicitly skips that health evidence.

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
