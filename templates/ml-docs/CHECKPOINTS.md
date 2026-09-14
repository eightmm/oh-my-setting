# CHECKPOINTS

Record the existing checkpoint format and loading contract, not a new schema.

- Save/load implementation and format revision:
- Model, optimizer, scheduler and random state actually stored:
- Code/config/data provenance needed to interpret or resume a run:
- Verified round-trip, resume and conversion checks:
- Inference compatibility versus exact training-resume compatibility:
- Retention/storage policy and approval needed for deletion:

Treat untrusted checkpoints as data, not executable input; preserve the
framework's safe loading path. Do not reset optimizer state silently or mark
all old checkpoints incompatible solely because a version label changed.
