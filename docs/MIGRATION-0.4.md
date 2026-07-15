# Migrating to 0.4

Version 0.4 makes install intent explicit and supports source refs as
reproducible entrypoints.

## Choose a channel

- Current: use the raw `main/install.sh` installer. Its default `edge` channel
  follows the repository's remote default branch.
- Reproducible pin: pass an exact tag, branch, or commit with `--ref`.

`--ref` overrides `OH_MY_SETTING_REF`; the environment overrides the embedded
installer default. A pin is checked out detached at an exact commit. `edge`
checks out and fast-forwards the remote default branch. An unknown or unsafe
ref fails before provider links or hooks are changed.

## Installation profiles

The default profile is `minimal`. `--full` records `full`; selecting individual
components such as `--tools` or `--auto-update` records `custom`. The concrete
component flags remain authoritative. Automation can set
`OH_MY_SETTING_PROFILE=minimal|full|custom` explicitly.

The schema-2 install receipt persists the selected ref and concrete component
choices. A plain later update therefore does not silently re-enable a Claude
hook, Codex plugin, or update timer that was disabled at install time. Existing
schema-1 receipts remain readable and are atomically rewritten on the first
successful 0.4 relink/update.

## Transactional updates

`oms update` now requires a clean canonical checkout. It records the previous
successful commit and reconciles links, hooks, plugin cache, and doctor before
committing the update. A link or doctor failure restores the previous HEAD,
links, and receipt. Use `oms update --check` for a read-only fetch comparison
and `oms update --rollback` to return to the recorded prior success.

## Removed compatibility surfaces

| Removed in 0.4 | Replacement |
|---|---|
| `multi-agent-ask.sh` | `oms peer-ask` |
| `multi-agent-review.sh` | `oms peer-review` |
| `multi-agent-delegate.sh` | `oms peer-delegate` |
| `OMS_MULTI_AGENT_*` | the matching `OMS_PEER_*` variable |
| `workflows/spec-first.md` | `spec-interview` skill |
| `workflows/slurm-hpc.md` | `slurm-hpc` skill |
| `workflows/new-server.md` | chat-driven installer and project bootstrap |

Legacy environment variables fail with an explicit replacement instead of
silently changing timeout behavior. During relink/uninstall, an OMS-owned
`~/.oh-my-setting-workflows` link is removed and its newest user backup is
restored; foreign links and regular files are preserved.

## Source install

The public installation path follows the repository source. Pin an audited
commit when reproducibility matters:

```bash
curl -fsSLO https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh
bash install.sh --ref <commit>
```

No `.oms` project data migration is required. Existing managed installs can
switch channel by rerunning an installer with the desired `--ref`.
