# Loss Reduction and Masking

Declare the optimization unit: sample, atom, residue, token, pair, positive, or
another explicit unit. Build validity masks from the data contract and apply
them before reduction.

- Never replace a missing target with zero unless zero is the documented label.
- Never average padded tensors without a mask.
- Keep the denominator visible and handle an all-invalid batch explicitly.
- For multitask data, track valid counts per head; a single dense mean can
  silently reweight tasks by label availability.
- Pooling, attention, graph readout, and metrics need the same validity boundary
  as the loss.

Single-process pattern:

```python
local_sum = (per_element_loss * valid_mask).sum()
local_count = valid_mask.sum()
if local_count.item() == 0:
    raise ValueError("batch has no valid training targets")
loss = local_sum / local_count
```

For DDP unequal counts, load the distributed reference directly from the parent
SKILL when distributed training is in scope.

Test the reduction against a small hand-computed batch containing padding,
missing labels, and unequal structure sizes.
