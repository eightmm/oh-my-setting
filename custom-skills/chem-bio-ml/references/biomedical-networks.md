# Biomedical Networks And Knowledge Graphs

Use for DDI, gene-disease, drug-target, pathway, repurposing, heterogeneous
biomedical graphs, knowledge-graph embeddings, and link prediction.

## Graph Construction

- Version every source database, ontology, identifier map, relation definition,
  evidence/provenance field, and graph-build transform.
- Resolve identifier aliases and entity merges before splitting. Preserve the
  mapping and do not merge biologically distinct isoforms, compounds, diseases,
  or relation semantics for convenience.
- Remove or mark inverse, symmetric, duplicate, entailed, hierarchy-derived,
  and metadata edges. A held-out edge trivially implied by a retained edge is
  leakage, not reasoning.
- Record literature and database dates. Text embeddings, retrieval corpora, and
  pretrained KGs may contain held-out or future edges.

## Split And Negatives

- Distinguish transductive edge holdout, inductive new-entity holdout,
  relation holdout, source holdout, and temporal forecasting. Match deployment.
- Group related assertions, reciprocal edges, duplicate sources, and ontology
  entailments on one side.
- Treat absent edges as unknown. Define closed-world assumptions and negative
  sampling by relation/domain/range; avoid impossible type combinations.
- Freeze validation/test negatives. Report degree and popularity distributions;
  random negatives often reward hub or type shortcuts.

## Features And Evaluation

- Fit graph statistics, node embeddings, text encoders/retrievers, and feature
  normalization without held-out/future edges. State any transductive access.
- Use filtered ranking only with a versioned known-positive set and report how
  newly discovered positives or false negatives are handled.
- Report relation-specific MRR/Hits@k or PR metrics, not only a micro average.
  Slice by node degree, evidence quality, source, time, and unseen entities.
- Compare type/degree/frequency, matrix-factorization, path/rule, and simple
  neighborhood baselines. Strong degree baselines expose shortcut benchmarks.
- Validate high-ranked hypotheses against independent evidence or a later
  snapshot; graph plausibility is not biological confirmation.

## Leakage Audit

- Check inverse and near-duplicate relations across splits.
- Check that node text does not literally state the held-out relation.
- Check ontology ancestors/descendants and derived paths that entail test edges.
- Check shared publications, curated assertions, and source-specific IDs across
  train/test.
- Check that evaluation candidates obey relation domain/range without making
  negatives trivially invalid.

Evidence anchor: [OpenBioLink](https://academic.oup.com/bioinformatics/article/36/13/4097/5825726)
builds a biomedical link-prediction benchmark around domain-specific graph
properties and explicit prevention of trivially inferable test statements.
