# Biologics And Immunology

Use for peptides, macrocycles, antibodies, antigens, peptide-MHC, epitopes,
biologic affinity, specificity, immunogenicity, and developability.

## Entity Definition

- For peptides, retain canonical/non-canonical residues, termini, cyclization,
  staples, disulfides, modifications, chirality, and assay form. Plain amino-
  acid strings can collapse distinct therapeutic entities.
- For antibodies, preserve paired heavy/light chains, chain type, germline,
  clonotype, species/humanization, numbering scheme, CDR definitions, format,
  and antigen/epitope identity.
- For MHC tasks, retain allele at the required resolution, peptide length,
  source protein, processing context, assay type, and species.
- Separate binding, presentation, cellular response, immunogenicity, and
  developability. They are related but not interchangeable labels.

## Split And Leakage

- Cluster peptides by sequence and chemical scaffold; cluster antibodies by
  clonotype/germline/CDR similarity and antigens by family/epitope.
- Report cold-peptide/antibody, cold-target/antigen, and cold-both settings.
- Keep variants from one parent campaign and measurements from one assay on one
  side for cross-campaign claims.
- Prevent source-protein leakage in epitope tasks and allele/motif memorization
  from dominating MHC evaluation. Report performance on rare/unseen alleles.
- Audit sequence databases, structure templates, and pretrained models for test
  families and future releases.

## Labels And Negatives

- Preserve affinity/kinetic units, censoring, assay platform, valency/avidity,
  temperature, pH, and format. Do not equate avidity with monovalent affinity.
- Treat non-observation as unknown. Record experimentally tested non-binders,
  shuffled/decoy peptides, or sampled antibody-antigen pairs separately.
- For developability, keep each endpoint distinct: expression, aggregation,
  solubility, viscosity, stability, polyreactivity, and immunogenicity.
- Model assay uncertainty and replicate disagreement; small affinity changes
  near assay noise should not become confident pairwise ranks.

## Evaluation And Design

- Include sequence/profile, nearest-parent, germline/clonotype, and simple
  physicochemical baselines.
- Report affinity ranking within antigen/campaign and generalization across
  antigens, not only pooled correlation.
- For MHC/epitope classification, report PR-AUC and allele/peptide-length
  slices; preserve the distinction between binding and presentation.
- For generated biologics, evaluate diversity, parent memorization, structural
  integrity, specificity/off-targets, and developability jointly. High predicted
  affinity alone is not a viable therapeutic design.

Evidence anchors: [TDC task taxonomy](https://tdcommons.ai/overview/) includes
peptide-MHC and antibody-affinity tasks; [AbBiBench](https://arxiv.org/abs/2506.04235)
evaluates antibody-antigen complexes as functional units rather than antibody
sequence plausibility alone.
