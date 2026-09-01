"""Graph engineering layer above the OMS control plane: schemas and vocabularies.

See docs/GRAPH-ENGINEERING.md for the implementation contract."""

GRAPH_PACKAGE_VERSION = "0.1.0"
EXEC_SCHEMA = 1
EVENTS_SCHEMA = 1
PROJECT_SCHEMA = 1
PARSER_VERSION = 2

OUTCOMES = ("completed", "failed", "unverified", "partial", "blocked", "changes_requested", "approved", "skipped")
NODE_KINDS = ("agent", "tool", "gate", "router", "subgraph", "terminal")
EFFECTS = ("read", "write", "none")
EDGE_KINDS = ("normal", "repeat")
JOINS = ("all", "any")
AGENT_MODES = ("run", "land")
GATE_AUTHORITIES = ("parent", "human")
EVENT_TYPES = ("run_started", "node_started", "node_outcome", "gate_decision", "route_evaluated", "run_finished", "note")
ROUTE_STATUSES = ("actionable", "waiting", "gate", "blocked", "exhausted", "terminal", "invalid")

CONFIDENCE = ("EXTRACTED", "INFERRED", "AMBIGUOUS")
RELATIONS = ("contains", "imports", "calls", "references", "depends_on", "uses", "produces", "configures", "validates", "tests", "part_of")
PROJECT_NODE_KINDS = ("file", "module", "class", "function", "method", "symbol", "test", "config", "document", "concept")
