# DATA

Data contract. Changes here invalidate checkpoints and baselines.

## Source

- Origin (URL/path):
- License:
- Version / snapshot date:
- Size (rows / GB):
- Hash (sha256 of canonical file):

## Schema

| Field | Type | Shape | Range | Notes |
|-------|------|-------|-------|-------|
|       |      |       |       |       |

## Labels / Targets

- Definition:
- Class balance:
- Known noise / ambiguity:

## Splits

- Train / Val / Test ratio:
- Split policy (random / time / group / stratified):
- Seed:
- Split file path:
- Leakage boundary (what MUST NOT cross splits):

## Preprocessing

- Pipeline version:
- Steps (in order):
  1.
- Stats fit on (train only? yes):
- Normalization params location:

## Verification

```bash
uv run python scripts/check_data.py
```

- Check shapes, dtypes, NaN/Inf, label range, split disjointness, hash match.

## Update Triggers

Source refresh, schema change, split policy change, preprocessing version bump -> update + bump checkpoint compatibility.
