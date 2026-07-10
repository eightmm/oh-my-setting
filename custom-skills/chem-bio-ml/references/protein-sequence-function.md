# Protein Sequence And Function

Use for sequence classification/regression, function and localization,
residue-level annotation, MSA/profile features, and protein language models.

## Intake And Identity

- Retain accession, database release, isoform, organism, taxon, construct,
  mutations, fragments, signal peptides, and evidence code.
- Normalize alphabet and case; flag ambiguous/non-canonical residues. Do not
  silently replace them with padding or a common amino acid.
- Distinguish full biological sequence from assayed construct. Map labels to the
  exact sequence used in the experiment.
- Deduplicate exact sequences before clustering. Preserve cluster algorithm,
  identity/coverage thresholds, parameters, and representative mapping.

## Split And Leakage

- Use sequence-identity clusters for novel-protein claims. Tighten to family,
  superfamily, domain architecture, taxon, or time splits when deployment needs
  those shifts.
- State identity and coverage thresholds. Identity alone can join short shared
  domains while the remaining sequence is unrelated.
- Keep isoforms, mutants, fragments, and homologous constructs together when
  they would make the evaluation trivial.
- Audit pretraining/retrieval leakage: a frozen pLM is allowed input only if the
  claim acknowledges its database cutoff; MSA/template searches must not index
  held-out labels or future database releases.

## Labels And Tasks

- Function labels are hierarchical and incomplete. Preserve ontology release,
  evidence code, annotation date, and ancestor propagation policy.
- Treat absent annotation as unknown unless a curated negative exists.
- For localization, define multi-label versus single-label behavior and handle
  signal peptides/transmembrane segments consistently.
- For residue tasks, map labels through insertions/deletions and mask unlabeled
  residues. Split by protein, never by residue rows.

## Features And Models

- For pLMs, pin model/revision, tokenizer, layer, max length, chunk overlap, and
  truncation policy. Exclude special/padding tokens from pooling.
- Run frozen embedding extraction under inference mode and verify that one
  sequence embedding is invariant to padding neighbors and batch composition.
- For MSA/PSSM/profile features, record database snapshot and search settings.
  Cache by sequence hash plus tool/database/parameter version.
- Compare with nearest-homolog transfer, k-mer/profile, and simple composition
  baselines before claiming representation gains.

## Metrics

- Multi-label function: micro/macro PR-AUC plus ontology-aware measures when
  relevant; report performance by annotation frequency and evidence strength.
- Imbalanced localization/classification: MCC, balanced accuracy, and PR-AUC.
- Residue prediction: per-protein and pooled metrics; report boundary tolerance
  for segment/interface tasks and never let long proteins dominate silently.
- Report metrics versus nearest-train sequence identity and domain/family shift.

Evidence anchor: [PEER](https://proceedings.neurips.cc/paper_files/paper/2022/hash/e467582d42d9c13fa9603df16f31de6d-Abstract-Datasets_and_Benchmarks.html)
covers function, localization, structure, protein-protein, and protein-ligand
tasks with engineered, sequence-encoded, and pretrained representations.
