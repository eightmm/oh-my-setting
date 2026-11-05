# Installing oh-my-setting

This guide is for people installing or maintaining OMS on Linux, macOS or Windows via WSL2.
Start with the command below, then use the host and profile details as needed.

The one-line install and what it changes are in the [README](../README.md).
This page holds the details: host requirements, capability profiles and
updates. [한국어](INSTALL.ko.md)

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## Windows via WSL2

Use **WSL2**, not WSL1. Keep both the repository and `~/.oh-my-setting`
on the Linux filesystem, along with your WSL home; do not use `/mnt/c`.
Install and run provider CLIs inside WSL, using the Linux requirements below.
Native Windows (Git Bash/MSYS2/Cygwin) is unsupported and OMS refuses to run.

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

## Installation details

The default installer selects the `core` capability: the harness, Bash, Git,
Python, and one coding-agent provider. It does not make GitHub CLI, Notion CLI,
all three providers, research tooling, or cluster tools mandatory. Use the
`full` compatibility profile only on machines that should carry the historical
all-provider/GitHub/Notion/research footprint. Nothing needs root except
installing a missing Git, tar, gzip or find; managed tool
bootstrap versions, platform URLs, and integrity values are pinned in `tools.lock.json`.
Fresh provider installs resolve a stable release into the private
`provider-tools.lock.json` snapshot. On Linux/macOS, fresh Claude downloads its
native binary and verifies SHA-256 from the vendor HTTPS manifest before
running `install VERSION`; OMS does not verify the manifest’s GPG signature.
Native Windows Git Bash keeps the verified npm Claude path. Fresh Codex and
agy keep their existing verified installation paths. The snapshot records
resolved versions, not a permanent version policy or proof of installation.

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

An explicit `OH_MY_SETTING_TOOL_LOCK` pins Claude with `claude install VERSION`
and requires equality. Native Codex and agy refuse a mismatched pin before
updating; matching pins leave them unchanged. Pi has no row in the current lock
schema, so an explicit lock skips Pi refresh with a note and leaves it unchanged.
On the default channel agy is vendor-managed after a lock-verified bootstrap,
like Claude and Codex: a fresh install leaves no ownership digest, and refresh
retires a legacy digest sidecar (warning when the bytes no longer match) only
while `~/.local` and `~/.local/bin` are real directories. Under an explicit
lock, doctor enforces the recorded digest and refresh refuses changed bytes
before running the binary. Restore the verified binary and marker, or preserve
both elsewhere and reapply the profile to install a verified payload. An interrupted OMS direct-install transaction
with recovery residue first recovers from a checksum-verified locked payload before vendor refresh.
Without transaction residue, unexpected owned bytes remain refused.
Doctor verifies marked commands before probes and skips any copy with an invalid
digest. Warning-only mode continues local state checks while skipping unsafe
tool/model/plugin/skill probes; required-tool and integration audits stop.

An absent, empty or invalid native versions directory leaves Claude on its
existing npm/shim path. Migration requires an executable numeric native release
inside the resolved vendor versions root.
When an OMS-owned Claude npm shim shadows existing native versions, OMS invokes
the newest native binary's installer to restore the vendor launcher. The proven
OMS shim moves to `.claude.oh-my-setting-migration` beside the launcher while
the vendor installs; failure restores it when the launcher is absent, and a
later lifecycle call verifies the native launcher directly, regardless of PATH
order, and finishes cleanup without installing again. The backup stays until
npm cleanup succeeds. Only the npm package in the prefix captured by that shim
is uninstalled, with lifecycle scripts disabled. An npm-owned Claude launcher
under the active npm root remains valid, including the `~/.local` prefix.
Unproven ownership refuses migration and names recovery;
a vendor launcher is never replaced by an OMS shim. A kill after the vendor
install but before npm cleanup can leave an extra npm copy. `oms doctor` lists
provider copies across PATH, vendor roots, and every nvm Node prefix, marks the
selected copy, channel and version, and gives prefix-specific npm uninstall
and foreign launcher removal commands. Only the command selected on PATH may
execute a version probe. All extra copies say "not executed", even with a
matching sidecar; npm versions come from package metadata. The `digest` status
is `matched`, `unmarked`, `invalid` or `unknown`. A matching sidecar describes
bytes and grants no execution authority. Vendor-retained
release files count as one installation and never receive cleanup guidance,
including when a release directory is on PATH. Aliases do not add version
probes. Cleanup guidance excludes the selected copy and its aliases.
Entries with line or field separators are escaped and reported as unknown
without execution. When an integrity failure disables probes, tool versions
are labeled "not probed". Doctor checks direct-tool transaction residue before
accepting a vendor-channel version.
Uninstall and repair leave vendor launchers and vendor roots in place.

Provider dependencies are selected with the provider: Codex and Claude include
locked Node, while Antigravity does not. New downloads are verified before use.
Existing external CLIs at the exact version are reused and labeled as
version-only by doctor. Install, update, repair, and uninstall share one
user-wide lifecycle lock.

Installation and scheduled updates use a private uv-managed Python pinned in
`tools.lock.json`; system/project Python is only a bootstrap, not the timer's
interpreter. Automatic updates preserve local checkout edits and report them as
`blocked`. Keep private policy in user skills outside the tracked install tree.
Systemd installs report whether logout persistence is available; explicitly set
`OH_MY_SETTING_AUTO_UPDATE_LINGER=1` when installing the trigger to enable it
(host permission may be required), or use cron. Linger applies to all this
account's user services, so OMS does not silently enable it by default.

Capability profiles are `core`, `council`, `github`, `notion`, `research`,
`hpc`, `container`, `remote`, and `full`. The selective installer reuses the
existing locked download transactions, records the exact requested profile in a
private receipt, and reapplies only that tool set during updates. Existing
installs without a capability receipt retain the legacy full-tool update path
until an agent explicitly migrates them. See
[MIGRATION.md](MIGRATION.md#capability-profiles). `oms install-profile --apply --add --profile github`
adds a capability while retaining the recorded profiles, provider, and degraded
policy. Without `--add`, `--profile` replaces the recorded selection; `--reapply`
reuses it exactly. `--plan` previews either operation without changing it.

`OH_MY_SETTING_MCP=0` and `OH_MY_SETTING_AGY_PLUGIN=0` skip their respective
integrations. Installation records both choices in the receipt; environment-clean
updates and scheduled triggers preserve them. An explicit environment override
on `oms update` changes the recorded choice. Antigravity also accepts `auto`
(the default: install only when available) or `1` (require it).

After refreshing provider tools, updates run a bounded integration audit of
Claude hooks, the Codex plugin, and enabled MCP/Antigravity interfaces. A failed
audit returns failure and names the successful core update separately; provider
binaries are outside the core rollback transaction. `--no-doctor` (or the
scheduled doctor opt-out) explicitly skips that health evidence.

`oms list` groups commands by intent: setup (`init`, `install-profile`), daily
work (`agent-plan`, `status`), collaboration, delivery, and maintenance
(`doctor`, `update`, `uninstall`). `oms list --all` retains the complete catalog.

When GitHub or Notion capabilities are selected, an interactive installer can
delegate browser login to `gh auth login` and `ntn login` and discover the Work
Journal's Notion target; a non-interactive install records the missing
capability instead of weakening the core runtime.
Claude Code gets compact main and subagent HUDs (model/effort, context,
rate-limit countdowns, cost, Git state) and a `high` effort default for Opus
5.5 unless you set one; Codex gets the equivalent native footer when it has no
user footer. The daily updater applies clean fast-forwards by
default and skips dirty or diverged checkouts. Uninstall restores managed
configuration but leaves the external CLIs and the user-local PATH entry in
place.

The install manages the three global agent rule files — `~/.claude/CLAUDE.md`,
`~/.codex/AGENTS.md`, and `~/.gemini/AGENTS.md`. A pre-existing file is moved
to `<file>.backup.<timestamp>` (announced at install time and reported by
`oms doctor`), stops applying while the install is active, and is restored by
`oms uninstall`.

| Host | Needs | Managed files |
|---|---|---|
| Linux (glibc) | Bash 3.2+, curl; Git, tar, gzip and find are installed if missing (root or sudo needed then). Python 3.11+ is provisioned as a managed runtime | symlinks |
| macOS | stock Bash 3.2, curl, Command Line Tools (`xcode-select --install`; without them `git` is only an installer stub). Python 3.11+ is provisioned as a managed runtime | symlinks |
| Windows via WSL2 | Linux requirements above, inside WSL2 | symlinks |

Python 3.11 or newer is required, with no upper version bound. The managed runtime stays on the 3.12 version in `tools.lock.json`, so older system Python on Ubuntu 20.04/22.04 or macOS Command Line Tools does not need replacing.

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

The awkward cases — Antigravity's headless permissions,
Notion data-source selection — live in
[COMPONENTS.md](COMPONENTS.md) and
[WORK-JOURNAL.md](WORK-JOURNAL.md). Upgrading an existing install
is one `oms update`; what each release changes underfoot is stated in its
migration note, currently [MIGRATION.md](MIGRATION.md#07).

Scripts live in `~/.oh-my-setting/scripts/` and are reachable as `oms <tool>`.
They are documented for transparency and recovery, not for manual use.
