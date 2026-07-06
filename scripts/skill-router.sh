#!/usr/bin/env bash
set -euo pipefail

# Claude Code UserPromptSubmit hook: deterministic skill routing. Skills often
# go un-invoked because nothing at prompt time reminds the model they exist;
# this matches the prompt against the trigger phrases in skills.manifest.json
# and prints a one-line hint (stdout becomes injected context). Precision over
# recall: at most OMS_ROUTER_MAX suggestions per prompt, each skill suggested
# at most once per session, silence on no match, and system-ish prompts
# (tool notifications, slash commands) are skipped entirely. Fail-open: this
# hook must never block a prompt.
#
# Disable with OMS_SKILL_ROUTER_OFF=1. Codex/Antigravity have no hook system;
# their equivalent is the skill picker plus the AGENTS.md skill-consult rule.

[ "${OMS_SKILL_ROUTER_OFF:-0}" = "1" ] && exit 0

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${OMS_SKILL_MANIFEST:-$ROOT/skills.manifest.json}"
[ -f "$MANIFEST" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# The heredoc owns python's stdin, so hand the hook payload over via env.
OMS_HOOK_PAYLOAD="$(cat)" OMS_ROUTER_MANIFEST="$MANIFEST" python3 <<'PY' || exit 0
import json, os, sys

try:
    data = json.loads(os.environ.get("OMS_HOOK_PAYLOAD") or "{}")
except Exception:
    sys.exit(0)

prompt = data.get("prompt") or ""
# Skip non-user prompts: injected notifications, XMLish payloads, slash
# commands (the skill is already being invoked), and trivial one-worders.
stripped = prompt.strip()
if not stripped or stripped.startswith(("<", "/")) or len(stripped) < 4:
    sys.exit(0)
low = stripped.lower()

try:
    manifest = json.load(open(os.environ["OMS_ROUTER_MANIFEST"], encoding="utf-8"))
except Exception:
    sys.exit(0)

scored = []
for s in manifest.get("skills", []):
    if not s.get("enabled") or not s.get("triggers"):
        continue
    hits = sum(1 for t in s["triggers"] if t.lower() in low)
    if hits:
        scored.append((-hits, s["name"]))
if not scored:
    sys.exit(0)
scored.sort()
max_n = int(os.environ.get("OMS_ROUTER_MAX", "2") or 2)
names = [n for _, n in scored[:max_n]]

# Once-per-session dedupe, keyed by the hook's session id.
session = str(data.get("session_id") or "nosession")[:64]
safe = "".join(c for c in session if c.isalnum() or c in "-_") or "nosession"
state_dir = os.path.join(
    os.environ.get("TMPDIR", "/tmp"), "oms-skill-router.%d" % os.getuid())
state = os.path.join(state_dir, safe)
seen = set()
try:
    with open(state, encoding="utf-8") as fh:
        seen = {line.strip() for line in fh}
except Exception:
    pass
fresh = [n for n in names if n not in seen]
if not fresh:
    sys.exit(0)
try:
    os.makedirs(state_dir, exist_ok=True)
    with open(state, "a", encoding="utf-8") as fh:
        for n in fresh:
            fh.write(n + "\n")
except Exception:
    pass  # dedupe is best-effort; still suggest

print("oh-my-setting skill hint: this request may match installed skill(s): "
      + ", ".join(fresh)
      + ". If relevant, invoke via the Skill tool before proceeding; ignore if not."
      + " (each skill hinted once per session)")
PY
