---
name: chem-bio-ml
description: >
  Discovery-focused chem-bio ML guardrails for small molecules, molecular 3D
  and physics, proteins and variants, biomolecular interactions, peptides and
  antibodies, reactions and synthesis, generative design, cellular omics and
  phenotypic profiling, and biomedical networks. Use before writing, reviewing,
  evaluating, or training on RDKit/SMILES/SELFIES, QSAR/ADMET, quantum or force
  data, MMseqs2/MSA/protein language models, PDB/AlphaFold/DMS, docking/DTI/PPI,
  peptide-MHC or antibody data, reaction/retrosynthesis/yield data, molecular or
  protein generation, RNA/oligonucleotide/CRISPR, single-cell perturbation/Cell
  Painting, or biomedical knowledge graphs. Enforce domain splits, leakage
  checks, label semantics,
  baselines, metrics, calibration, applicability domain, and provenance.
---

# Chem-Bio ML Guardrails

Treat silent scientific errors as release blockers. Classify the task before
choosing a representation or split; the same rows require different holdout
keys for warm-start, cold-entity, temporal, or prospective use.

## Workflow

1. Define the prediction unit, input entities, label, deployment question, and
   allowed information at inference time.
2. Read [shared-data-evaluation.md](references/shared-data-evaluation.md) for
   every task.
3. Read every task-family reference that applies. Interaction and multimodal
   tasks usually require more than one.
4. Write the split keys, label transformations, baseline, primary metric, and
   out-of-domain policy in `PROJECT.md` before training.
5. Add executable assertions at the data/model boundary. Do not treat a prose
   checklist as verification.
6. Run `oms data-manifest check --name <manifest>` and
   `oms data-manifest leakage --name <manifest>` when split files exist, then
   run focused domain assertions. Use `oms peer-review --ml` only when the user
   requests cross-agent review or at an explicit release/pre-training gate.

## Reference Router

| Task family | Read |
|---|---|
| Shared provenance, labels, splits, metrics, calibration | [shared-data-evaluation.md](references/shared-data-evaluation.md) |
| QSAR, ADMET, bioactivity, fingerprints, SMILES, molecular graphs | [small-molecule-property.md](references/small-molecule-property.md) |
| Conformers, quantum properties, energies/forces, molecular dynamics | [molecular-3d-physics.md](references/molecular-3d-physics.md) |
| Protein function/localization, residue tasks, MSA/profile/pLM | [protein-sequence-function.md](references/protein-sequence-function.md) |
| PDB/predicted structures, pockets, DMS/fitness | [protein-structure-variant.md](references/protein-structure-variant.md) |
| DTI, docking, affinity, PPI, protein-nucleic-acid complexes | [interactions-complexes.md](references/interactions-complexes.md) |
| Peptides, MHC, antibodies, antigens, biologic developability | [biologics-immunology.md](references/biologics-immunology.md) |
| RNA/DNA, genomic/regulatory variants, oligonucleotides, CRISPR | [nucleic-acids-gene-editing.md](references/nucleic-acids-gene-editing.md) |
| Reaction prediction, yield, catalyst, retrosynthesis | [reactions-synthesis.md](references/reactions-synthesis.md) |
| Molecule/protein/peptide generation and optimization | [generative-design.md](references/generative-design.md) |
| Bulk/single-cell perturbation, drug combination, Cell Painting | [cellular-omics-phenotypic.md](references/cellular-omics-phenotypic.md) |
| DDI, gene-disease, pathway and biomedical KG/link prediction | [biomedical-networks.md](references/biomedical-networks.md) |

## Multi-Family Rules

- Protein-ligand affinity: read molecule, protein, interaction, and shared.
- Protein variant effects: read protein sequence, protein structure/variant,
  and shared; structure is optional input, not an assumed requirement.
- Genomic/regulatory variant effects: read nucleic-acid and shared; add cellular
  when labels depend on cell type, tissue, perturbation, or expression context.
- Structure-conditioned antibody design: read protein structure, interaction,
  biologics, generation, and shared.
- CRISPR outcome/off-target prediction: read nucleic-acid, cellular, and shared.
- Chemical perturbation response: read molecule, cellular, and shared.
- Reaction-conditioned molecular generation: read reaction, generation,
  molecule, and shared.
- Knowledge-graph-assisted DTI: read network, interaction, both entity
  references, and shared. Prevent source-text or edge leakage across all views.

## Split Manifest Contract

Precompute domain keys as columns. Pass only keys that the declared evaluation
must hold out; a warm-start benchmark may intentionally share entities, while a
cold-both benchmark must not.

```bash
oms data-manifest create --name study-v1 --id-column example_id \
  --key-column split_group --key-column entity_or_pair_key \
  --split train=splits/train.csv --split val=splits/val.csv --split test=splits/test.csv
oms data-manifest leakage --name study-v1
oms data-manifest check --name study-v1
```

Use standardized molecule IDs, sequence/family clusters, assay/campaign IDs,
donor/batch IDs, reaction templates, or graph entity/time keys as appropriate.
The manifest checks supplied keys; it does not compute chemistry or biology.

## Global Stop Conditions

Stop before training when any of these is unknown:

- inference-time information boundary or deployment population;
- standardization/deduplication order and immutable entity IDs;
- split unit, group keys, temporal cutoff, or source/database snapshot;
- label units, direction, censoring, replicate policy, or negative provenance;
- train-only fit boundary for normalization, vocabulary, features, thresholds,
  templates, retrieval indexes, and hyperparameter selection;
- cheap domain baseline, primary metric, subgroup slices, and OOD policy;
- feature/model version parity between training and inference.

For experimental hypotheses and result interpretation, also load
`research-method`. For optimizer, DDP, precision, checkpoint, or equivariant
architecture defaults, also load `ml-training`.
