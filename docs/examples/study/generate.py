"""Offline example of a user-supplied generation CLI wrapper."""
import json
from pathlib import Path
import sys

case = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
prediction = {"answer": case["input"]["x"] + case["params"]["offset"]}
Path(sys.argv[2]).write_text(json.dumps(prediction), encoding="utf-8")
