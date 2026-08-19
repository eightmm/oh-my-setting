---
name: oms-dataset-safety
description: Register datasets and splits in a manifest and check group leakage and split drift before training: declared group keys (not row identity) decide train/eval separation, so the same protein, patient, or source never sits on both sides.
---

# Dataset Safety

Group leakage found after training is the most expensive bug in an ML
project. The manifest check is cheap; run it first.

1. **Register before first use**: `oms data-manifest` records a dataset or
   split by IDs and declared group keys — no raw rows are stored. Declare
   the group key that actually defines contamination (protein family,
   patient, source document), not just the row ID.
2. **Check before training**: `oms data-manifest check --name <manifest>`
   then `oms data-manifest leakage --name <manifest>` whenever registered
   splits exist. A run that starts before the leakage check passes is a
   result you may have to retract.
3. **Re-check on change**: regenerating a split, adding data, or changing
   preprocessing invalidates the old verdict — re-run the manifest check;
   it also catches split drift against the recorded state.
4. **Refuse silent splits**: when manifests exist in the repo, do not train
   on an unregistered split; register it first so the check has something
   to verify.
