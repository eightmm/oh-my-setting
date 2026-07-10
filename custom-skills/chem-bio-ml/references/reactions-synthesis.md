# Reactions And Synthesis

Use for forward reaction prediction, reaction classification, atom mapping,
yield/condition/catalyst prediction, retrosynthesis, and route planning.

## Reaction Record

- Preserve source/license, publication or patent family, date, procedure,
  reactant/reagent/solvent/catalyst roles, stoichiometry, concentrations,
  temperature/time, atmosphere, workup, product identity, yield type, and units.
- Keep missing conditions as unknown. Patent-derived records often omit failed
  reactions and procedural details; do not interpret absence as a negative.
- Standardize structures without erasing atom mapping, isotope labels,
  stereochemistry, charge, or role information needed by the task.
- Validate atom balance and mapping. Record mapper/version/confidence and keep
  mapping outside the input when it would reveal the answer at inference.

## Deduplication And Splits

- Deduplicate repeated literature/patent reactions and near-identical condition
  screens before splitting. Group patent families, publications, campaigns,
  reaction templates/classes, substrates, and products as required.
- Use temporal/source holdouts for prospective claims. Random reaction rows can
  put the same transformation and substrate series on both sides.
- For retrosynthesis, prevent product or route overlap and close analog routes
  from leaking across splits. Define whether the target is one-step disconnection
  or a complete route.
- Keep all condition-screen rows from one experimental campaign together.

## Labels And Bias

- Distinguish isolated, assay, conversion, selectivity, and reported yields.
  Zero, failed, trace, and unreported yields are different observations.
- Preserve regio-, chemo-, and stereoselectivity. Canonical strings that drop
  stereo can score a scientifically wrong product as correct.
- Publication and patent data overrepresent successful reactions. Do not claim
  failure prediction without measured failures or a defensible negative design.
- Track reaction scale and lab/platform; yield models can learn campaign or
  apparatus signatures rather than chemistry.

## Evaluation

- Forward/retro prediction: top-k exact and stereo-aware accuracy plus chemical
  validity, atom balance, and class/source/time slices. Report canonicalization
  and equivalence rules.
- Yield/condition prediction: MAE/RMSE/rank correlation with campaign and
  temporal slices; include mean-by-class and simple descriptor baselines.
- Route planning: route success, length/cost, diversity, building-block
  availability, and execution budget. One-step top-k does not validate routes.
- Compare template/retrieval and nearest-reaction baselines to neural models.

Evidence anchor: [Open Reaction Database](https://pubs.acs.org/doi/10.1021/jacs.1c09820)
defines structured reaction provenance for prediction, synthesis planning, and
experimental design and highlights incomplete retrospective records.
