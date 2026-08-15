#!/usr/bin/env python3
"""Compatibility wrapper for the exact-base OMS runtime overlay installer."""
from __future__ import annotations
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from apply_to_oms_core import ApplyError, main

if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ApplyError as exc:
        print("apply-to-oms: %s" % exc, file=sys.stderr)
        raise SystemExit(2)
