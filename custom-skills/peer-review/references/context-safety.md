# Review Context and Safety

Read `git status --short`, the relevant diff/base, and the verification result
before calling reviewers. Include goal, changed files, contract, known risks,
and only the context needed to judge the patch.

Exclude env/credential/key/certificate files, SSH/cloud auth state, machine and
cluster snapshots, datasets, checkpoints, raw logs, local scratch, and
project-private paths. If excluded content is essential, review it locally and
tell peers only that private context was omitted.

Check provider availability without installing or authenticating anything.
Unavailable providers are skipped and reported. If every provider is blocked,
perform a current-agent review and state that no independent signal was
obtained.

Use `--diff` for uncommitted work and `--base origin/main` or another explicit
base for branch/PR review. Add untracked files to the diff boundary only after
confirming they are safe to transmit.
