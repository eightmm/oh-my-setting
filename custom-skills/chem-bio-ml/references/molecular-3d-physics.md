# Molecular 3D, Quantum, And Physics

Use for conformer-dependent properties, quantum chemistry, energies/forces,
interatomic potentials, molecular dynamics, and 3D geometric learning.

## Coordinate And Label Provenance

- Record coordinate source: experimental, optimized quantum geometry, MD frame,
  predicted structure, docking pose, or generated conformer.
- Record protonation, tautomer, charge, multiplicity, conformer generation,
  optimization method, solvent, temperature, and energy model.
- For quantum labels, pin method/basis/functional, reference energy convention,
  units, and convergence criteria. Do not merge fidelities as one target unless
  the model and evaluation explicitly represent fidelity.
- For forces, define sign and units and verify `force = -grad(energy)` on a
  small batch when the architecture claims energy conservation.

## Split And Sampling

- Split by parent molecule/system before frames or conformers. Random frame
  splits leak almost identical geometries from the same trajectory.
- Remove temporal neighbors or use blocked trajectory splits. Report effective
  independent sample count, not raw frame count.
- Hold out chemistry, composition, conformational basin, temperature/pressure,
  or system size according to the intended extrapolation.
- Keep generated conformers from one molecule on one side. Evaluate sensitivity
  across a seeded conformer ensemble rather than one lucky geometry.

## Geometry Contract

- Assert one coordinate and target unit convention, such as Å and eV/Å.
- Center only when translation should be removed. Never use ligand-defined
  frames when the ligand is unavailable at screening time.
- Apply proper rotations by default and transform vector/tensor targets with
  coordinates. Use reflections only for parity-safe, non-chiral tasks with the
  correct polar/axial/tensor transformation; never mirror chiral entities as a
  generic augmentation.
- Test invariant scalar outputs and equivariant vector outputs under random
  rigid transforms. Test permutation behavior for identical atom types.
- Define periodic boundary handling, neighbor-list cutoff/skin, image wrapping,
  and cutoff continuity for periodic or condensed-phase systems.

## Evaluation

- Report energy and force errors separately and by element, geometry regime,
  energy range, molecule/system, and in/out-of-domain slice.
- For dynamics, evaluate stability, conserved quantities, structural
  distributions, and rollout drift; one-step force MAE is insufficient.
- Compare against 2D and simple physics baselines. A 3D model must justify its
  coordinate cost and coordinate-quality dependence.
- For crystal-to-generated-conformer transfer, report the distribution shift
  and metric variance across conformers.

Evidence anchors: [ATOM3D](https://datasets-benchmarks-proceedings.neurips.cc/paper/2021/hash/c45147dee729311ef5b5c3003946c48f-Abstract-round1.html)
benchmarks multiple biomolecular 3D tasks; [MoleculeNet](https://pubs.rsc.org/en/content/articlelanding/2018/sc/c7sc02664a)
shows that physics-aware featurization can dominate model choice for quantum and
biophysical targets.
