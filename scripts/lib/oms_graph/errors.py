"""Typed failure for the graph layer; exit code follows the runtime core."""

from __future__ import annotations

from oms_runtime.common import CoreError


class GraphError(CoreError):
    """Contract violation, invalid spec, or refused write (exit 2 unless given)."""
