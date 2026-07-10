# Cellular, Omics, And Phenotypic Profiling

Use for bulk or single-cell perturbation response, CRISPR or chemical screens,
drug combinations, transcriptomics/proteomics/epigenomics, and Cell Painting or
other high-content phenotypic assays.

## Experimental Unit And Provenance

- Identify the independent unit: donor, animal, organoid, culture, plate, well,
  perturbation, batch, cell, image site, or acquisition run. Cells and image
  fields from one well are not independent replicates.
- Retain donor/line, tissue, disease state, passage, batch, plate/well/site,
  perturbation identity, dose, duration, vehicle/control, protocol, instrument,
  and processing pipeline.
- Preserve raw counts/images and QC decisions. Version feature extraction,
  segmentation, gene sets, normalization, and reference atlases.
- Prevent pseudoreplication: compute uncertainty and statistical tests over
  biological units, not millions of cells or crops.
- For spatial assays, keep sections/tiles/spots from one specimen, patient, or
  slide together and retain coordinates, capture region, and tissue hierarchy.

## Split And Confounding

- Choose holdouts from deployment: unseen perturbation, dose, cell line, donor,
  tissue, batch/lab, combination, or time. Report each axis separately.
- Keep wells/cells/sites from one experimental unit together. Random cell or
  image splits leak batch, plate, donor, and perturbation identity.
- For drug combinations, distinguish unseen pair from unseen constituent drugs
  and cold-both. Preserve dose matrix and synergy definition.
- Fit normalization, feature selection, highly-variable genes, batch correction,
  PCA, and reference mappings without test information unless the method is
  explicitly transductive and reported as such.
- Audit whether perturbations are confounded with plates, batches, cell lines,
  or acquisition settings. A model can predict layout instead of biology.
- Treat omics missingness as potentially informative and not at random. Record
  detection/QC thresholds, library depth, composition, and per-unit cell count;
  do not silently zero-fill absent measurements.

## Modality-Specific Identity

- Proteomics: retain peptide-spectrum identity, search database, FDR, shared or
  unique peptide status, protein-inference rule, acquisition batch, and protein
  group. Split by biological specimen, not peptide or spectrum.
- Metabolomics: retain feature, isotope/adduct, formula, compound/isomer identity,
  annotation-confidence level, retention time, ionization mode, and batch.
  Prevent the same compound's correlated adducts or isotopes crossing splits.
- Spatial transcriptomics: report specimen/patient-level metrics and spatial
  autocorrelation; neighboring spots or crops are not independent samples.

## Labels And Evaluation

- Distinguish measured state, differential response, mechanism, viability,
  synergy, and downstream phenotype. Define control and counterfactual.
- Avoid raw-expression correlation dominated by unchanged genes. Evaluate
  perturbation deltas, differential genes/pathways, direction, magnitude, and
  distributional or rank metrics at the biological-unit level.
- Include no-change, control mean, nearest perturbation, additive combination,
  linear, and simple batch-aware baselines.
- Report by donor/line, perturbation class, dose, effect size, batch, and unseen
  context. Check calibration and uncertainty under context shift.
- Do not make causal claims from observational associations or unbalanced
  perturbation designs without identification assumptions and controls.

## Imaging Guards

- Separate plates/wells/sites before crop extraction. Remove duplicate fields
  and avoid selecting focus/QC thresholds on test phenotypes.
- Measure plate and batch predictability from embeddings. Strong nuisance
  prediction without correction/stratification invalidates biological claims.
- Report per-well/compound metrics and replicate concordance, not crop-level
  accuracy alone.

Evidence anchors: [scPerturb](https://www.nature.com/articles/s41592-023-02144-y)
harmonizes perturbation-response datasets across molecular readouts;
[JUMP Cell Painting](https://jump-cellpainting.broadinstitute.org/) provides a
large public morphological-profiling resource for chemical and genetic effects.
