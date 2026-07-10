# Nucleic Acids And Gene Editing

Use for DNA/RNA sequence or structure, genomic and regulatory variant effects,
miRNA/siRNA/ASO and other oligonucleotide therapeutics, RNA-ligand tasks,
CRISPR guide efficiency, off-target prediction, and gene-editing outcomes.

## Entity And Context

- Preserve molecule type, strand orientation, alphabet, reference assembly and
  transcript release, isoform, genomic coordinates, chemistry/modifications,
  termini, length, secondary-structure conditions, and delivery context.
- For genomic variants, verify reference alleles against the pinned assembly;
  normalize strand, left alignment, and minimal representation before assigning
  immutable variant IDs. Preserve original representation and liftover logs.
- Represent multi-allelic loci explicitly. Do not let alternate alleles,
  equivalent indel encodings, overlapping windows, or haplotype-derived views
  of one locus cross split boundaries.
- For CRISPR, record nuclease/variant, PAM, guide scaffold, target locus/allele,
  cell type, delivery, dose/time, assay, edit type, and outcome definition.
- Map all coordinates and variants to one declared genome/transcript version.
  Liftover and transcript choice can change the target and label.
- Treat modified oligonucleotides as chemical entities; plain base strings can
  collapse different backbones, sugars, stereochemistry, and conjugates.

## Split And Leakage

- Group overlapping windows, reverse complements, guides for the same locus,
  transcript isoforms, homologous genes, and close sequence/structure clusters.
- Match deployment with unseen-guide, unseen-locus/gene, unseen-cell-context,
  unseen-nuclease/PAM, chromosome, species, or temporal/source holdouts.
- For population or individual-level variant data, group relatives and repeated
  individuals; audit ancestry, cohort/site, allele frequency, and population
  structure. Random individuals can leak pedigrees or LD-correlated haplotypes.
- For regulatory/splicing effects, hold out loci, genes, or regulatory elements
  and block overlapping or high-LD sequence windows when claiming new-locus
  generalization. Report by ancestry/cohort and allele-frequency bin.
- For off-target tasks, prevent the same guide-target pair or near-identical
  genomic sites from crossing splits. Keep measured candidate panels together.
- Fit k-mer vocabularies, folding/accessibility features, genomic annotations,
  chromatin tracks, and retrieval indexes without test loci or future releases.
- Audit pretrained sequence models and reference databases for future assembly,
  annotation, or assay-derived leakage.

## Labels And Negatives

- Distinguish cleavage, indel/edit spectrum, desired edit, viability, expression
  knockdown, binding, stability, delivery, toxicity, and functional phenotype.
- Preserve count depth, detection limit, editing baseline, replicate, assay,
  and cell context. Compositional outcome vectors require an explicit simplex
  or count model rather than independent unconstrained regressions.
- Unmeasured genomic sites are unknown. Record experimentally tested negatives,
  candidate-generation rules, and detection thresholds for off-target labels.
- For RNA structure, distinguish experimental probing, comparative annotation,
  and predicted folding labels; predicted structures are features or pseudo-
  labels, not equivalent ground truth.

## Features And Evaluation

- Version tokenization, reverse-complement handling, folding tool/parameters,
  ionic/temperature conditions, accessibility, epigenomic tracks, and chemical
  modification encoding.
- Include GC/content, motif/rule, nearest-guide, thermodynamic/folding, and
  simple locus/context baselines.
- Report PR-AUC and recall at fixed validation budget for sparse off-targets;
  calibration and false-negative sensitivity are essential.
- Report guide/locus/gene/context slices and cold-axis results. For editing
  spectra, evaluate distributional distance and scientifically relevant outcome
  frequencies, not only aggregate accuracy.

Evidence anchor: [TDC task taxonomy](https://tdcommons.ai/overview/) includes
miRNA-target interaction and CRISPR outcome tasks alongside small molecules and
biologics.
