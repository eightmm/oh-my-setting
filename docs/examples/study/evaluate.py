"""One evaluator shared by every case and variant in the offline example."""
import json
from pathlib import Path
import sys

case = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
prediction = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
metrics = {"error": abs(prediction["answer"] - case["expected"]),
           "cost": 1 + abs(case["params"]["offset"])}
Path(sys.argv[3]).write_text(json.dumps({"metrics": metrics}), encoding="utf-8")
