#!/usr/bin/env python3
"""Thin entrypoint for the OMS graph layer (mirrors oms_core.py)."""

from __future__ import annotations

from oms_graph.cli import main


if __name__ == "__main__":
    raise SystemExit(main())
