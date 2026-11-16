# oms-command-panel

Native terminal panel command adapter. Panel reentry and job supervision resolve
`oms` in this order: the current executable when named `oms`, an executable
sibling `oms`, an existing `scripts/oms` under the supplied source root, then
an executable `oms` on `PATH`. Script directories and search directories are
canonicalized before changing working directory; the `oms` leaf is preserved,
including when it is a symlink to a versioned binary. Execute access is checked
with the OS, rather than inferred from mode bits. Interactive reentry uses
`bash` for the legacy script and accepts non-executable scripts. Job supervision
requires executable scripts and skips other scripts in favor of `PATH`.
Missing candidates produce an unavailable executable error before room creation,
state updates or tmux mutations. Each lifecycle entry resolves once and reuses
that prefix for its launch and watch commands.

`OMS_PANEL_JOB_EXECUTABLE` overrides supervisor resolution when nonempty and
must identify an executable file (bare command names are checked on `PATH`).
The override is passed through verbatim after validation and does not override
interactive panel reentry.
Native dispatcher, hooks, lifecycle, landing and Pi harness integration paths
receive the same high-risk recommendation as their legacy script counterparts.

Wave 3 hosts must supply the real source root when a legacy script is intended,
install sibling binaries together or expose `oms` on `PATH`, and retain the
native producer integrations described in the panel-core README. Resolution
performs reads only and creates no record, ref or file. Concurrent calls resolve
independently; interruption publishes nothing. Executable replacement between
resolution and process creation remains subject to ordinary OS spawn behavior.

Resolution regressions live in `tests/support/executable_resolution.rs` and run
as library tests with isolated subprocess environments and temporary Git repos.
`tests/panel_parity.rs` retains the end-to-end recommendation and CLI contracts.
