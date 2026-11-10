---
name: oms-chem-validity
description: Check molecular data before training or reporting on it: parse failures, duplicates after standardization, salt and stereo policy, label units and censoring, replicate handling, how split keys were computed, pose validity next to RMSD, and dataset licenses.
---

# Molecular Data Validity

A model trained on unparsed, duplicated or mixed-unit molecules reports a
number nobody can interpret. These checks are cheap next to a training run.
OMS ships no chemistry library: each check is the project's own script (RDKit
or whatever the project already uses), and OMS records that it ran.

1. **Parsing**: count the structures the toolkit fails to parse and report
   the rate. Decide what happens to them (drop, fix, fail) and write it in
   `DATA.md`; never drop silently.
2. **Standardize, then deduplicate**: state the salt-stripping, charge,
   tautomer and stereo policy. Count duplicates by a standardized key
   (InChIKey, or the first block when stereo is ignored) inside each split
   and across splits. Duplicates across splits are leakage.
3. **Labels**: one unit per label column (nM against uM, pIC50 against IC50),
   converted before any merge. Keep censored values (`>`, `<`) as censored or
   exclude them by a stated rule; a qualifier turned into a plain number is a
   wrong label. Say how replicates are aggregated and how conflicting
   replicates are treated.
4. **Split keys**: scaffold, cluster or target keys come from a script. Keep
   that script in the repo, record its path beside the manifest, and register
   the computed keys with `oms data-manifest` so `leakage` checks the declared
   key. A random split of a congeneric series is not a held-out test.
5. **Poses**: report physical validity (bond lengths, clashes, chirality,
   for example a PoseBusters pass rate) beside RMSD. A low RMSD on an invalid
   pose is not a success.
6. **Licenses**: record each dataset's license and whether its splits may be
   redistributed; some benchmarks separate commercial from academic use.

Make each check a command that exits non-zero on failure and list it in the
experiment contract's `invariant_pack`, so `oms runtime experiment invariants`
runs it and the verdict depends on it. A check that was not run is unknown,
not passed: say which ones were skipped and why.
