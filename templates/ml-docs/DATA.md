# DATA

Data contract. Assess which results and checkpoints a changed contract affects.

## Source

- Origin (URL/path):
- License:
- Version / snapshot date:
- Assay/protocol/organism/construct context:
- Size (rows / GB):
- Hash (sha256 of canonical file):

## Schema

| Field | Type | Shape | Range | Notes |
|-------|------|-------|-------|-------|
|       |      |       |       |       |

## Labels / Targets

- Definition:
- Units and direction:
- Censoring / detection limits:
- Replicate aggregation policy:
- Negative source (measured inactive / decoy / unmeasured / other):
- Class balance:
- Known noise / ambiguity:

## Splits

- Train / Val / Test ratio:
- Split policy (random / time / group / stratified):
- Prediction and independent evaluation unit:
- Standardized entity IDs:
- Group keys (entity/pair/scaffold/family/assay/donor/time/etc.):
- Seed:
- Split file path:
- Data manifest name:
- Leakage boundary (what MUST NOT cross splits):

## Preprocessing

- Pipeline version:
- Steps (in order):
  1.
- Stats fit on (train only? yes):
- Other train-only fitted state (vocabulary/features/clusters/index/thresholds):
- Normalization params location:

## Verification

- Existing data validation command and observed result:

- Check shapes, dtypes, NaN/Inf, label range, split disjointness, hash match.

## Update Triggers

For source, schema, split or preprocessing changes, record the affected claims
and compatibility evidence. Preserve historical snapshots and results.
