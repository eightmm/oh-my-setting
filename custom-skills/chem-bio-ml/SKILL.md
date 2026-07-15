---
name: chem-bio-ml
description: >
  Scientific guardrails for chemical and biological ML data, splits, labels,
  representations, evaluation, and provenance. Use for molecule, protein,
  interaction, biologic, reaction, generation, omics, gene-editing, or
  biomedical-network modeling tasks.
---

# Chem-Bio ML Guardrails

Classify the prediction unit and deployment question before choosing a split,
representation, or metric. Treat silent scientific errors as blockers.

## Workflow

1. Define input entities, output label, inference-time information boundary,
   immutable IDs, and intended generalization.
2. For datasets, splits, training, or evaluation, read
   [shared-data-evaluation.md](references/shared-data-evaluation.md). Simple
   parsing, format conversion, or library use need only the relevant family
   reference and source documentation.
3. Read each applicable task-family reference. Multimodal work usually needs
   the entity, interaction, and output-family references together.
4. Record split keys, label transformations, baseline, metric, calibration/OOD
   policy, and provenance in `PROJECT.md` before compute depends on them.
5. Add executable boundary assertions. When registered splits exist, run
   `oms data-manifest check` and `oms data-manifest leakage`.

## Reference Router

| Task family | Read |
|---|---|
| Shared data, splits, labels, metrics, calibration | [shared-data-evaluation.md](references/shared-data-evaluation.md) |
| QSAR, ADMET, SMILES, molecular graphs | [small-molecule-property.md](references/small-molecule-property.md) |
| Conformers, quantum, energies/forces, MD | [molecular-3d-physics.md](references/molecular-3d-physics.md) |
| Protein sequence, function, residue tasks, MSA/pLM | [protein-sequence-function.md](references/protein-sequence-function.md) |
| Protein structure, pockets, DMS/fitness | [protein-structure-variant.md](references/protein-structure-variant.md) |
| DTI, docking, affinity, PPI, complexes | [interactions-complexes.md](references/interactions-complexes.md) |
| Peptides, MHC, antibodies, antigens | [biologics-immunology.md](references/biologics-immunology.md) |
| RNA/DNA, genomic variants, oligos, CRISPR | [nucleic-acids-gene-editing.md](references/nucleic-acids-gene-editing.md) |
| Reactions, yield, catalyst, retrosynthesis | [reactions-synthesis.md](references/reactions-synthesis.md) |
| Molecule/protein/peptide generation | [generative-design.md](references/generative-design.md) |
| Single-cell, perturbation, Cell Painting | [cellular-omics-phenotypic.md](references/cellular-omics-phenotypic.md) |
| DDI, gene-disease, pathways, biomedical KG | [biomedical-networks.md](references/biomedical-networks.md) |

Stop before training when inference boundaries, split/group keys, source
snapshot, label semantics, train-only fitting, baseline/metric, or feature
version parity is unknown. Load `research-method` for experiments and
`ml-training` for optimizer, distributed, precision, checkpoint, or
equivariance work.
