# Generative Design

Use for molecule, protein, peptide, antibody, sequence, graph, or 3D generation
and goal-directed optimization.

## Define The Generation Problem

- Distinguish distribution learning, unconditional generation, conditional
  design, constrained optimization, inverse design, and search/ranking.
- Define the generated entity completely: stereochemistry, charge, sequence
  modifications, 3D geometry, complex partner/context, and synthesizability.
- Freeze training corpus, standardization, deduplication, oracle versions, and
  evaluation budget. Remove evaluation targets and close parents from training.

## Memorization And Validity

- Report exact and near-neighbor memorization against train, validation,
  reference databases, and known design campaigns.
- Validity, uniqueness, and novelty are necessary but insufficient. Add internal
  diversity, coverage, distribution fidelity, scaffold/family novelty, and
  nearest-neighbor similarity.
- Validate chemistry/biology after decoding. Syntax-valid SMILES or plausible
  protein likelihood does not establish stable structure or function.
- Report failure and filter rates before and after sanitization; do not score
  only the survivors.

## Goal-Directed Evaluation

- Use held-out oracles when possible. Optimizing and evaluating with the same
  learned predictor rewards oracle exploitation and adversarial artifacts.
- Report all objectives and constraints, not a weighted scalar alone. Include
  Pareto fronts and baseline starting points for multi-objective design.
- Include synthesis/developability, diversity, uncertainty, and off-target or
  counter-screen constraints. A single high affinity/property score is not a
  useful design.
- Fix oracle-call, compute, and wall-clock budgets. Compare against random,
  retrieval, virtual-screening, genetic/search, and local-edit baselines.
- Re-score with orthogonal methods and flag out-of-domain candidates. Treat
  docking and surrogate scores as hypotheses, not experimental validation.

## 3D And Conditional Design

- Prevent conditioning leakage from bound ligands, future structures, or test
  labels. State what pocket/partner information exists prospectively.
- Test rigid-transform behavior, clashes, geometry, conformer diversity, and
  pose/sequence consistency.
- For protein/biologic design, separate sequence plausibility, fold confidence,
  binding, specificity, and developability; report parent/template similarity.

## Evidence And Handoff

- Keep generated candidates, scores, oracle versions, seeds, and filtering
  decisions. Report the full candidate funnel and selection rule.
- Require prospective synthesis/assay before claiming discovery or improved
  function. Retrospective surrogate gains are prioritization evidence only.

Evidence anchor: [GuacaMol](https://pubs.acs.org/doi/10.1021/acs.jcim.8b00839)
separates distribution-learning and goal-directed benchmarks and evaluates
novelty, chemical-space behavior, quality, and multi-objective optimization.
