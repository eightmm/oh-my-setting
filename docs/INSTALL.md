# Installing oh-my-setting

The one-line install is in the [README](../README.md). This page holds the
details: host requirements, capability profiles, updates and what the
installer changes on your machine.

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

On Windows, a fresh default Codex setup — or any profile selecting a
Node-backed tool — first needs native **Node.js with npm**. Install the exact
release named by the installer from the [official Node.js
archive](https://nodejs.org/dist/), keep its PATH option enabled, and reopen Git
Bash. The installer stops instead of accepting a different Node version. An
Antigravity-only `core` profile does not need Node.

The default installer selects the `core` capability: the harness, Bash, Git,
Python, and one coding-agent provider. It does not make GitHub CLI, Notion CLI,
all three providers, research tooling, or cluster tools mandatory. Use the
`full` compatibility profile only on machines that should carry the historical
all-provider/GitHub/Notion/research footprint. Nothing needs root; managed tool
bootstrap versions, platform URLs, and integrity values are pinned in `tools.lock.json`.
Codex, Claude, and agy resolve current stable releases during install/update,
then verify downloaded bytes against their official release checksums. Scheduled
apply refreshes installed providers even when OMS itself is unchanged. Manual
`--no-tools` skips tool refresh; an explicit `OH_MY_SETTING_TOOL_LOCK` retains
reproducible pinned installs. The private `provider-tools.lock.json` snapshot
records resolved versions, not a permanent version policy or proof of installation.
Recognized standalone Codex installations keep their native updater; OMS checks
the resulting version without replacing the launcher with npm or claiming an
OMS-verified payload digest.
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
[OMS-RUNTIME.md](OMS-RUNTIME.md).

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
| Linux, WSL (glibc) | Bash 3.2+, curl; Git, tar, gzip and find are installed if missing (root or sudo needed then). No Python needed | symlinks |
| macOS | stock Bash 3.2, curl, Command Line Tools (`xcode-select --install`; without them `git` is only an installer stub). No Python needed | symlinks |
| Windows Git Bash | Git, Python 3.9+; exact locked native Node when selected tools need it | verified copies |

With no `python3` 3.9+ and no uv, the installer fetches the pinned uv
(sha256-checked), builds its own Python from `tools.lock.json`, and points a
managed `python3` shim at it. musl-based Linux such as Alpine is not
supported: the pinned builds need glibc.

The awkward cases — Windows copy mode, Antigravity's headless permissions,
Notion data-source selection — live in
[COMPONENTS.md](COMPONENTS.md) and
[WORK-JOURNAL.md](WORK-JOURNAL.md). Upgrading an existing install
is one `oms update`; what each release changes underfoot is stated in its
migration note, currently [MIGRATION-0.7.md](MIGRATION-0.7.md).

Scripts live in `~/.oh-my-setting/scripts/` and are reachable as `oms <tool>`.
They are documented for transparency and recovery, not for manual use.
