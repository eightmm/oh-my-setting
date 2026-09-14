# DEBUGGING

Keep only project-specific diagnoses worth reusing; generic debugging policy
already belongs to the agent's shared guidance.

For a recurring failure, record the symptom, competing causes, discriminating
probe, confirmed cause and verified fix with source/test references.

Do not silently reset optimizer state, change precision, pad/truncate inputs,
or replace a scientific objective to hide a failure. Separate environment
failures from model defects and preserve the original reproduction evidence.
