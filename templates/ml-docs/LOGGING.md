# LOGGING

Record the project's chosen local logs or tracker; no external service is
required or enabled by this template.

- Run identifier and log/artifact locations:
- Metric definitions, units and evaluation split:
- Configuration/provenance recorded with results:
- Resume identity and distributed logging ownership:
- Metrics file (optional, read by `oms job-digest --metrics FILE`): JSONL, one
  object per line `{"step": 120, "name": "train/loss", "value": 0.31,
  "ts": 1767225600}`; `ts` is epoch seconds or ISO-8601. DVCLive
  `plots/metrics/*.tsv` is read as is. A file read, not a watcher:
- External export, redaction and network policy, if applicable:

Link to existing logging configuration. Keep secrets and private inputs out of
published logs; do not duplicate every metric key here.
