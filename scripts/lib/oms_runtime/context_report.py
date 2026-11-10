"""Human-readable views of bounded context projections; no collection or writes."""
from __future__ import annotations

import unicodedata

MAX_LABEL_CHARS = 160
SCOPE_REASONS = {
    "different-phase": "different workflow phases",
    "different-targets": "different target sets",
    "contract-identity-unavailable": "contract identity unavailable",
    "different-contract-state": "different recorded contract states",
}


def _label(value):
    # JSON permits line/paragraph separators; plain terminal output must not.
    text = "".join(" " if ch.isspace() else "?" if unicodedata.category(ch)[0] == "C"
                   else ch for ch in value)
    if len(text) > MAX_LABEL_CHARS:
        text = text[:MAX_LABEL_CHARS] + "... [label shortened]"
    return text or "(empty)"


def _section(lines, title, group, indent=""):
    lines.append(f"{indent}{title}: {group['total']} total; {len(group['items'])} shown; "
                 f"{group['omitted']} not shown")


def _paths(lines, title, group, indent=""):
    _section(lines, title, group, indent)
    lines.extend(f"{indent}  - {_label(path)}" for path in group["items"])


def render_report(report):
    """Render inspect_files' internal projection, not arbitrary input JSON.

    Existing collectors own validation and list bounds. This view preserves
    their counts, unknowns and ordering, and never changes the projection.
    """
    if type(report.get("schema")) is not int or report["schema"] != 1:
        raise ValueError("unsupported context report schema")
    kind = report.get("kind")
    if kind == "oms-context-inspection":
        lines = _inspection(report)
    elif kind == "oms-context-comparison":
        lines = _comparison(report)
    else:
        raise ValueError("unsupported context report kind")
    lines.extend(["", "Read-only view; no bundle verification or acceptance judgment.",
                  "No token, quota or billing savings are inferred.",
                  "Long labels are marked; use JSON for full metadata. Metadata may be private."])
    return "\n".join(lines)


def _inspection(report):
    debt, budget = report["debt"], report["budget"]
    lines = ["OMS context inspection (recorded metadata)",
             f"Phase: {_label(report['phase'])}; recorded at: {_label(report['generated_at'])}", "",
             f"Required context debt (recorded): {debt['reported']}",
             "Recorded sufficient: " + ("yes" if debt["reported_sufficient"] else "no")]
    # Truncated required paths are a subset of missing-required, not extra debt.
    _paths(lines, "Missing or incomplete required sources", debt["missing_required"])
    _paths(lines, "Truncated required sources (subset above)", debt["truncated_required"])
    lines.append("")
    current = report["sources_current"]
    if current is None:
        lines.append("Current sources: not checked (use --verify-current to read selected sources)")
    else:
        checked = sum(current[key] for key in ("unchanged", "changed", "missing", "unreadable"))
        lines.append(f"Current sources (read-time): {checked} selected; "
                     f"{current['unchanged']} unchanged; {current['changed']} changed; "
                     f"{current['missing']} missing; {current['unreadable']} unreadable")
        _paths(lines, "Changed source paths", current["changed_paths"])
        _paths(lines, "Missing source paths", current["missing_paths"])
        lines.append("Read-time hashes are not an atomic snapshot or a validation of the bundle.")
    lines.extend(["", "Recorded byte budget (not tokens)",
                  f"  Used {budget['used_bytes']} B / {budget['bytes']} B; "
                  f"remaining {budget['remaining_bytes']} B",
                  f"  Source payload {budget['source_payload_bytes']} B; "
                  f"framing {budget['framing_bytes']} B", ""])
    _paths(lines, "Targets", report["targets"])
    _section(lines, "Selected sources (largest recorded payload first)", report["sources"])
    for row in report["sources"]["items"]:
        lines.extend([f"  - {_label(row['path'])}",
                      f"    {row['bytes_selected']} B selected / {row['bytes_before']} B original; "
                      f"{row['selection']}; {row['omitted_bytes']} B omitted",
                      f"    Reason: {_label(row['reason'])}",
                      f"    Source SHA-256: {row['sha256']}"])
    lines.append("Selected payload can include the compiler's truncation marker.")
    lines.append("")
    _section(lines, "Omitted sources", report["omissions"])
    for row in report["omissions"]["items"]:
        size = "unreported" if row["bytes"] is None else f"{row['bytes']} B"
        lines.extend([f"  - {_label(row['path'])} (size {size})",
                      f"    Reason: {_label(row['reason'])}"])
    lines.append("")
    _section(lines, "Repeated full-source hashes (candidates only)", report["repeated_source_digests"])
    for row in report["repeated_source_digests"]["items"]:
        lines.append(f"  SHA-256: {row['sha256']}")
        _paths(lines, "Paths", row["paths"], "  ")
    lines.extend(["Same source hash does not prove the selected slices duplicate one another.", "",
                  f"Recorded bundle SHA-256: {report['bundle_sha256']}",
                  f"Recorded contract digest: {report['contract_state_digest'] or 'unavailable'}",
                  "This is an OMS context pack, not the provider's full context."])
    return lines


def _comparison(report):
    lines = ["OMS context comparison (recorded metadata; right minus left)", ""]
    if report["recorded_scope_matches"]:
        lines.append("Recorded scope: matches; current task/repository identity is not verified.")
    else:
        lines.append("Recorded scope: different or unknown; do not infer a winning run.")
        lines.extend("  - " + _label(SCOPE_REASONS.get(reason, reason))
                     for reason in report["scope_mismatch_reasons"])
    delta = report["delta"]
    lines.extend(["", "Recorded deltas (right - left)"])
    for key, label in (("budget_bytes", "Budget"), ("selected_bytes", "Used"),
                       ("remaining_bytes", "Remaining")):
        lines.append(f"  {label}: {delta[key]:+d} B")
    lines.extend([f"  Required context debt: {delta['context_debt']:+d}", ""])
    _paths(lines, "Added selected sources", report["added"])
    _paths(lines, "Removed selected sources", report["removed"])
    _section(lines, "Changed selected-source metadata", report["changed"])
    for row in report["changed"]["items"]:
        lines.extend([f"  - {_label(row['path'])}",
                      "    Fields: " + ", ".join(_label(field) for field in row["changed_fields"])])
    lines.append("Zero deltas do not prove identical packs, quality or current validity.")
    return lines
