# Equivariant Models

Use the framework already chosen by the project. Adding cuEquivariance, e3nn,
or another dependency requires explicit approval and a compatibility check.

Test symmetry instead of inferring it from layer names:

- rotate and translate raw coordinates before any alignment shortcut;
- invariant outputs such as energy must remain unchanged;
- equivariant vectors such as force must rotate with the input;
- use float64 for the oracle so reduced precision does not hide an error;
- cover batching, masks, periodic boundaries, and any coordinate centering used
  in production.

An absolute-coordinate projection, uncentered global feature, incorrect tensor
product layout, or preprocessing alignment can make a model train normally
while violating the intended SE(3) contract. Run the transformation test at the
raw network boundary and at the public inference boundary.
