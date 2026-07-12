# Experiment Coordination

Claim an expensive experiment before launch so multiple agents do not duplicate
it. The study board records intent; the ledger/capsule records execution; run
reconciliation records terminal scheduler state.

```bash
oms experiment-board claim --id scaffold --hypothesis "scaffold split helps"
oms experiment-board start --id scaffold --job <job-id>
oms experiment-board finish --id scaffold --result "AUC 0.82 vs 0.74"
oms experiment-board list
```

Mint one run ID and let tools link their records:

```bash
id=$(oms run new --note "scaffold split")
export OMS_RUN_ID="$id"
oms experiment-board claim --id scaffold --hypothesis "scaffold helps"
oms run-capsule run --config c.yaml -- uv run python train.py
oms run show "$id"
```

Touch long-lived claims so recovery does not treat a live owner as stale. Use
`oms run-reconcile` after Slurm jobs finish. Reclaim only after checking owner,
heartbeat, scheduler state, and artifacts.
