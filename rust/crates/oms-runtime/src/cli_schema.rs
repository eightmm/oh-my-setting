//! Reference cli_parser.py SHA-256: 4a0023aee10a12265dbf92d2dac091f53612936535d250e8902f8d9e77621ae0
//! Argument/help metadata transcribed from cli_parser.py; no parser is executed.
pub(super) const SCOPES: &str = r###"[
  {"path":[],"description":"Typed semantic core over the existing hardened OMS execution plane.",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":".","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","help":"pretty-print JSON"},
    {"names":["--version"],"action":"version","version":"0.7.0"}
   ],"children":[{"name":"envelope","help":"project the effective TaskEnvelope"},{"name":"evidence","help":"criterion-level evidence coverage"},{"name":"next","help":"return the smallest currently valid internal actions"},{"name":"brief","help":"read a return brief, optionally comparing a saved envelope"},{"name":"failure","help":"classify failures and inspect recovery metadata"},{"name":"context","help":"compile a bounded context manifest and bundle"},{"name":"context-inspect","help":"inspect saved context metadata and optionally verify source files"},{"name":"profile","help":"optional capability contracts"},{"name":"release","help":"stable/edge update channel handling"},{"name":"capsule","help":"sanitized cross-machine continuity state"},{"name":"backend","help":"trusted-local, isolated, or remote execution"},{"name":"experiment","help":"ExperimentContract v2 and invariant packs"},{"name":"benchmark","help":"harness effectiveness telemetry"},{"name":"doctor","help":"validate the runtime projection without replacing OMS doctor"}],"groups":[]},
  {"path":["envelope"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["action"],"choices":["show","write"],"nargs":"?","default":"show"},
    {"names":["--output"],"default":".oms/runtime/effective-contract.json"}
   ],"children":[],"groups":[]},
  {"path":["evidence"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"show","help":""},{"name":"unbound","help":""},{"name":"matrix","help":"read criterion/task/evidence links without inferring missing ones"},{"name":"bind","help":""},{"name":"revoke","help":""}],"groups":[]},
  {"path":["evidence", "show"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["evidence", "unbound"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["evidence", "matrix"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--task"],"help":"filter by an exactly recorded task id"},
    {"names":["--limit"],"type":"int","default":20,"help":"rows per list, 1..100"},
    {"names":["--format"],"choices":["json","text"],"default":"json"}
   ],"children":[],"groups":[]},
  {"path":["evidence", "bind"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--criterion"],"required":true},
    {"names":["--ref"],"required":true},
    {"names":["--status"],"required":true,"choices":["verified","failed","inconclusive","skipped_with_reason","stale"]},
    {"names":["--type"],"default":"explicit-binding"},
    {"names":["--note"],"default":""},
    {"names":["--depends"],"action":"append","default":[]}
   ],"children":[],"groups":[]},
  {"path":["evidence", "revoke"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--binding"],"required":true},
    {"names":["--reason"],"default":""}
   ],"children":[],"groups":[]},
  {"path":["next"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--explain"],"action":"store_true","help":"read task blockers and canonical action reasons"},
    {"names":["--task"],"help":"with --explain: select an exactly recorded plan task"},
    {"names":["--limit"],"type":"int","default":20,"help":"with --explain: rows per list, 1..100"},
    {"names":["--format"],"choices":["json","text"],"default":"json","help":"with --explain only"}
   ],"children":[],"groups":[]},
  {"path":["brief"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--before"],"help":"explicit prior envelope inside the selected repository; no auto-discovery or writes"},
    {"names":["--limit"],"type":"int","default":20,"help":"rows per list, 1..100"},
    {"names":["--format"],"choices":["json","text"],"default":"json"}
   ],"children":[],"groups":[]},
  {"path":["failure"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"classify","help":""},{"name":"catalog","help":""}],"groups":[]},
  {"path":["failure", "classify"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["text"]},
    {"names":["--exit"],"type":"int","default":null},
    {"names":["--code"],"default":""}
   ],"children":[],"groups":[]},
  {"path":["failure", "catalog"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["context"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--target"],"action":"append","default":[]},
    {"names":["--file"],"action":"append","default":[],"help":"PATH[:reason]"},
    {"names":["--require"],"action":"append","default":[]},
    {"names":["--max-bytes"],"type":"int","default":65536},
    {"names":["--bundle"],"default":""},
    {"names":["--manifest"],"default":""},
    {"names":["--allow-external"],"action":"store_true"},
    {"names":["--phase"],"default":"implementation","choices":["orientation","implementation","verification","review","research"]}
   ],"children":[],"groups":[]},
  {"path":["context-inspect"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["manifest"],"nargs":"?","help":"existing manifest inside the selected repository"},
    {"names":["--injections"],"action":"store_true","help":"instead of a manifest: bytes OMS hooks injected into sessions, from the hook event ledger"},
    {"names":["--session-hash"],"help":"with --injections: one recorded session hash"},
    {"names":["--session"],"metavar":"TRANSCRIPT","help":"instead of a manifest: per-tool result bytes and repeated reads in one Claude Code JSONL transcript"},
    {"names":["--timeline"],"action":"store_true","help":"with --session: ordered tool calls (name, recorded duration, error flag, byte sizes, first error row); no arguments or outputs; up to 200 steps, --limit does not apply"},
    {"names":["--compare"],"help":"compare with another saved manifest; never run either pack"},
    {"names":["--verify-current"],"action":"store_true","help":"compare current source hashes (incompatible with --compare)"},
    {"names":["--limit"],"type":"int","default":10,"help":"rows per displayed list, 1..100 (default: 10)"},
    {"names":["--format"],"choices":["json","text"],"default":"json","help":"output format (default: json); text is a read-only report"}
   ],"children":[],"groups":[]},
  {"path":["profile"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"list","help":""},{"name":"current","help":""},{"name":"show","help":""},{"name":"check","help":""},{"name":"apply","help":""},{"name":"install-plan","help":""},{"name":"install","help":""}],"groups":[]},
  {"path":["profile", "list"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["profile", "current"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["profile", "show"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["name"],"choices":["container","core","council","full","github","hpc","notion","remote","research"]}
   ],"children":[],"groups":[]},
  {"path":["profile", "check"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profiles"],"nargs":"+","choices":["container","core","council","full","github","hpc","notion","remote","research"]}
   ],"children":[],"groups":[]},
  {"path":["profile", "apply"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profiles"],"nargs":"+","choices":["container","core","council","full","github","hpc","notion","remote","research"]},
    {"names":["--allow-missing"],"action":"store_true"}
   ],"children":[],"groups":[]},
  {"path":["profile", "install-plan"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profiles"],"nargs":"+","choices":["container","core","council","full","github","hpc","notion","remote","research"]},
    {"names":["--primary-provider"],"default":"codex","choices":["codex","claude","agy"]}
   ],"children":[],"groups":[]},
  {"path":["profile", "install"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profiles"],"nargs":"+","choices":["container","core","council","full","github","hpc","notion","remote","research"]},
    {"names":["--primary-provider"],"default":null,"choices":["auto","codex","claude","agy"]},
    {"names":["--upgrade"],"action":"store_true"},
    {"names":["--allow-missing"],"action":"store_true"},
    {"names":["--receipt"],"default":""},
    {"names":["--dry-run"],"action":"store_true"}
   ],"children":[],"groups":[]},
  {"path":["release"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"status","help":""},{"name":"resolve","help":""},{"name":"apply","help":""},{"name":"promote","help":""}],"groups":[]},
  {"path":["release", "status"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["release", "resolve"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["channel"],"choices":["stable","edge"]},
    {"names":["--fetch"],"action":"store_true"}
   ],"children":[],"groups":[]},
  {"path":["release", "apply"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["channel"],"choices":["stable","edge"]},
    {"names":["--no-fetch"],"action":"store_true"},
    {"names":["--tools"],"action":"store_true"}
   ],"children":[],"groups":[]},
  {"path":["release", "promote"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--commit"],"required":true},
    {"names":["--version"],"required":true},
    {"names":["--expected-manifest-digest"],"required":true}
   ],"children":[],"groups":[]},
  {"path":["capsule"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"export","help":""},{"name":"verify","help":""},{"name":"import","help":""},{"name":"diff","help":""}],"groups":[]},
  {"path":["capsule", "export"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--output"],"default":""}
   ],"children":[],"groups":[]},
  {"path":["capsule", "verify"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["path"]}
   ],"children":[],"groups":[]},
  {"path":["capsule", "import"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["path"]}
   ],"children":[],"groups":[]},
  {"path":["capsule", "diff"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["left"]},
    {"names":["right"]}
   ],"children":[],"groups":[]},
  {"path":["backend"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"target","help":"local SSH target metadata; never exported in capsules"},{"name":"describe","help":""},{"name":"check","help":""},{"name":"run","help":""}],"groups":[]},
  {"path":["backend", "target"],"description":"Stored in .oms/runtime/targets.json, excluded from capsules. Existing names are refused; remove before adding a replacement. Status and git sync use SSH; command execution is not yet supported.",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"add","help":"register metadata only; validates alias syntax without reading SSH config"},{"name":"list","help":"read local targets without creating state"},{"name":"remove","help":"remove one existing local target"},{"name":"approve","help":"approve execution for the current target identity"},{"name":"revoke","help":"revoke execution approval"},{"name":"status","help":"inspect checkout"},{"name":"sync","help":"clone or fast-forward through origin; push first"}],"groups":[]},
  {"path":["backend", "target", "add"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["name"],"help":"unique local name with the same syntax as --ssh-alias"},
    {"names":["--ssh-alias"],"required":true,"help":"1..64 letters, digits, dots, underscores or hyphens; starts with a letter or digit"},
    {"names":["--workdir"],"help":"optional ~/REL; must equal the resolved local repository path under HOME"},
    {"names":["--capability"],"action":"append","default":[]},
    {"names":["--scheduler"],"choices":["none","slurm"],"default":"none"},
    {"names":["--scratch-root"]}
   ],"children":[],"groups":[]},
  {"path":["backend", "target", "list"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["backend", "target", "remove"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["name"]}
   ],"children":[],"groups":[]},
  {"path":["backend", "target", "approve"],"description":"",
   "arguments":[{"names":["-h","--help"],"action":"help","help":"show this help message and exit"},{"names":["--repo"],"default":"SUPPRESS"},{"names":["--pretty"],"action":"store_true","default":"SUPPRESS"},{"names":["name"]}],"children":[],"groups":[]},
  {"path":["backend", "target", "revoke"],"description":"",
   "arguments":[{"names":["-h","--help"],"action":"help","help":"show this help message and exit"},{"names":["--repo"],"default":"SUPPRESS"},{"names":["--pretty"],"action":"store_true","default":"SUPPRESS"},{"names":["name"]}],"children":[],"groups":[]},
  {"path":["backend", "target", "status"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["name"]},
    {"names":["--timeout"],"type":"int","default":120,"help":"timeout in seconds, 10..7200; default: %(default)s"}
   ],"children":[],"groups":[]},
  {"path":["backend", "target", "sync"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["name"]},
    {"names":["--timeout"],"type":"int","default":1800,"help":"timeout in seconds, 10..7200; default: %(default)s"},
    {"names":["--sha"],"help":"full 40- or 64-hex commit SHA already pushed to origin; defaults to HEAD"}
   ],"children":[],"groups":[]},
  {"path":["backend", "describe"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profile"],"choices":["trusted-local","isolated","remote"]},
    {"names":["--image"],"default":""},
    {"names":["--adapter"],"default":"","group":0}
   ],"children":[],"groups":[{}]},
  {"path":["backend", "check"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profile"],"choices":["trusted-local","isolated","remote"]},
    {"names":["--image"],"default":""},
    {"names":["--adapter"],"default":null,"group":0},
    {"names":["--target"],"help":"remote target; offline check always reports unready until SSH execution is supported","group":0}
   ],"children":[],"groups":[{}]},
  {"path":["backend", "run"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["profile"],"choices":["trusted-local","isolated","remote"]},
    {"names":["--worktree"],"default":""},
    {"names":["--image"],"default":""},
    {"names":["--adapter"],"default":null,"group":0},
    {"names":["--target"],"help":"remote target; execution is not yet supported","group":0},
    {"names":["--allow-network"],"action":"store_true"},
    {"names":["--inherit-env"],"action":"append","default":[]},
    {"names":["--timeout-seconds"],"type":"int","default":1200},
    {"names":["--log"],"default":""},
    {"names":["--receipt"],"default":""},
    {"names":["--log-limit"],"type":"int","default":4194304},
    {"names":["--memory-mb"],"type":"int","default":4096},
    {"names":["--cpus"],"type":"float","default":2.0},
    {"names":["--pids-limit"],"type":"int","default":512},
    {"names":["--tmpfs-mb"],"type":"int","default":1024},
    {"names":["remainder"],"nargs":"+"}
   ],"children":[],"groups":[{}]},
  {"path":["experiment"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"launch","help":"pre-register one host command in the run ledger"},{"name":"template","help":""},{"name":"validate","help":""},{"name":"compare","help":""},{"name":"register","help":""},{"name":"show","help":""},{"name":"run","help":""},{"name":"summarize","help":""},{"name":"sweep","help":"enumerate a hyperparameter grid as named cells; runs and submits nothing"},{"name":"evaluate","help":""},{"name":"invariants","help":""}],"groups":[]},
  {"path":["experiment", "launch"],"description":"Provide all seven claim fields or --contract PATH, then -- COMMAND. Uses run-ledger and its project verification gate; preserves command output/exit. Paths resolve from --repo. Do not put secrets in recorded fields or command arguments.",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--question"],"default":""},
    {"names":["--hypothesis"],"default":""},
    {"names":["--prediction"],"default":""},
    {"names":["--baseline"],"default":""},
    {"names":["--metric"],"default":""},
    {"names":["--success"],"default":""},
    {"names":["--change"],"default":""},
    {"names":["--contract"],"default":"","help":"validated ExperimentContract v2; conflicting field overrides are refused"},
    {"names":["--metrics"],"default":"","help":"metrics JSON emitted by the command"},
    {"names":["--file"],"default":"","help":"ledger path; default docs/EXPERIMENTS.jsonl"},
    {"names":["--reason"],"default":"","help":"recorded justification required with --no-gate"},
    {"names":["--no-gate"],"action":"store_true","help":"skip project verification; requires --reason"},
    {"names":["--dry-run"],"action":"store_true","help":"validate without launching or writing the ledger"},
    {"names":["remainder"],"nargs":"REMAINDER"}
   ],"children":[],"groups":[]},
  {"path":["experiment", "template"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--output"],"default":""}
   ],"children":[],"groups":[]},
  {"path":["experiment", "validate"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["contract"]}
   ],"children":[],"groups":[]},
  {"path":["experiment", "compare"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["left"]},
    {"names":["right"]}
   ],"children":[],"groups":[]},
  {"path":["experiment", "register"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["contract"]},
    {"names":["--id"],"default":""}
   ],"children":[],"groups":[]},
  {"path":["experiment", "show"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["id"]}
   ],"children":[],"groups":[]},
  {"path":["experiment", "run"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--id"],"required":true},
    {"names":["--arm"],"required":true,"choices":["baseline","treatment"]},
    {"names":["--seed"],"required":true,"type":"int"},
    {"names":["--metrics"],"required":true},
    {"names":["--profile"],"default":"trusted-local","choices":["trusted-local","isolated","remote"]},
    {"names":["--image"],"default":""},
    {"names":["--adapter"],"default":""},
    {"names":["--worktree"],"default":""},
    {"names":["--timeout-seconds"],"type":"int","default":86400},
    {"names":["--allow-existing-metrics"],"action":"store_true"},
    {"names":["--reason"],"default":"","help":"recorded justification for a run past the contract budget.runs"},
    {"names":["remainder"],"nargs":"+"}
   ],"children":[],"groups":[]},
  {"path":["experiment", "summarize"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--id"],"required":true}
   ],"children":[],"groups":[]},
  {"path":["experiment", "sweep"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"plan","help":""}],"groups":[]},
  {"path":["experiment", "sweep", "plan"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["sweep"],"help":"JSON file or literal: {\"grid\": {...}, \"seeds\": [...], \"fixed\": {...}}"},
    {"names":["--emit"],"default":"","choices":["","tsv","hydra-overrides","sbatch-array"],"help":"print the runs as text instead of the JSON plan"}
   ],"children":[],"groups":[]},
  {"path":["experiment", "evaluate"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["contract"]},
    {"names":["results"]}
   ],"children":[],"groups":[]},
  {"path":["experiment", "invariants"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["contract"]},
    {"names":["--profile"],"default":"trusted-local","choices":["trusted-local","isolated","remote"]},
    {"names":["--image"],"default":""},
    {"names":["--adapter"],"default":""},
    {"names":["--timeout-seconds"],"type":"int","default":1200}
   ],"children":[],"groups":[]},
  {"path":["benchmark"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[{"name":"show","help":""},{"name":"snapshot","help":""},{"name":"record","help":""},{"name":"compare","help":""}],"groups":[]},
  {"path":["benchmark", "show"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["benchmark", "snapshot"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"}
   ],"children":[],"groups":[]},
  {"path":["benchmark", "record"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--task-id"],"required":true},
    {"names":["--status"],"required":true,"choices":["verified","failed","partial","blocked"]},
    {"names":["--human-corrections"],"type":"int","default":0},
    {"names":["--escaped-defects"],"type":"int","default":0},
    {"names":["--reverted-lines"],"type":"int","default":0},
    {"names":["--false-refusals"],"type":"int","default":0},
    {"names":["--duplicate-work"],"type":"int","default":0},
    {"names":["--note"],"default":""}
   ],"children":[],"groups":[]},
  {"path":["benchmark", "compare"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["left"]},
    {"names":["right"]}
   ],"children":[],"groups":[]},
  {"path":["doctor"],"description":"",
   "arguments":[
    {"names":["-h","--help"],"action":"help","help":"show this help message and exit"},
    {"names":["--repo"],"default":"SUPPRESS","help":"project repository or directory"},
    {"names":["--pretty"],"action":"store_true","default":"SUPPRESS","help":"pretty-print JSON"},
    {"names":["--strict"],"action":"store_true"}
   ],"children":[],"groups":[]}
]"###;
