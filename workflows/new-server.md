# New Server Workflow

> Deprecated: use the installed `oh-my-setting-ops` skill and `doctor.sh`.
> This compatibility file will be removed in the next minor release.

1. Install `curl` manually if the server does not have it.
2. Run the install command from `README.md`.
3. Run `~/.oh-my-setting/scripts/doctor.sh`.
4. Login to each CLI: `claude`, `codex`, `agy`.
5. Confirm Python uses `uv`: `uv --version`.
6. On HPC machines, confirm Slurm if available: `sinfo`, `squeue -u "$USER"`.
7. Add real secrets outside this repo.
