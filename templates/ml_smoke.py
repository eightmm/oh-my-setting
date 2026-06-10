"""One-batch ML interface smoke. CPU-only, under 60 seconds.

Run via the verification contract:

    bash scripts/check.sh ml-smoke

Fill each step for this project and keep failures loud — a smoke that
cannot fail protects nothing. Delete steps that truly do not apply.
"""

import sys


def main() -> int:
    # TODO 1. config: load the default training config from configs/.
    # TODO 2. data: build the dataset/dataloader and pull exactly one batch.
    # TODO 3. model: construct the model on CPU.
    # TODO 4. forward + loss: run one forward pass and compute the loss.
    # TODO 5. backward: loss.backward(); assert gradients are finite.
    # TODO 6. eval mode: model.eval() forward runs without error.
    # TODO 7. checkpoint: save + load a checkpoint round-trip.
    print(
        "ml_smoke: not configured yet; fill the TODO steps in scripts/ml_smoke.py",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
