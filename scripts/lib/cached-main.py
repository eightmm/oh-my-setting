"""Run a Python script as __main__ through the bytecode cache.

CPython recompiles a file run by path on every start but reuses __pycache__
for an imported module: 15ms per work_journal.py start and 9ms per
hook_state.py start, paid on every prompt and Stop hook (2026-10-01).
"""
import os
import runpy
import sys

path = sys.argv.pop(1)
sys.path[0] = os.path.dirname(os.path.abspath(path))
runpy.run_module(os.path.basename(path)[:-3], run_name="__main__", alter_sys=True)
