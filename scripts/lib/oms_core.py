#!/usr/bin/env python3
"""Thin entrypoint for the modular OMS runtime core."""

from __future__ import annotations

from oms_runtime.cli import main


if __name__ == "__main__":
    raise SystemExit(main())
