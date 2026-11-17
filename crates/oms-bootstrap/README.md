# Native frontdoor preparation

This standalone library supplies the read-only bootstrap for the parent's OMS
binary. It has no public executable, competing command registry, runtime
interpreter, runtime Git invocation, download, build, or lifecycle fallback.
The workspace and lock are local to this crate; generated output is ignored
under `/target/`. All dependencies already exist in the repository.

## Parent integration contract

The parent owns `scripts/oms`, `crates/oms/src/main.rs`, its `registry.rs`,
workspace membership, staging, lifecycle writers, inventory, and central docs.
This patch alone does not make an installed invocation Python-free.

1. Before starting native code, the shell trampoline authenticates the selected
   native executable against reviewed staging metadata. The trampoline cannot
   use a receipt-selected executable to authenticate itself. Establish the
   authority root and protect its descendants before trusting the checksum;
   verifying only the binary's own post-startup report is insufficient.
2. Pass arguments as distinct OS strings with the private interface
   `--oms-frontdoor ENTRY -- ARGS...`. `ENTRY` is the dispatcher path, including
   an installed symlink or an unrendered COPY dispatcher. There may be no args.
   `PrivateInvocation::parse` parses this interface and the existing
   `--oms-select ROOT REVISION -- TOOL...` and
   `--oms-dispatch ROOT REVISION -- TOOL...` interfaces. Parsing grants no proof.
3. Implement `CommandAdapter` in the parent: `is_public` uses its existing
   registry metadata, `owns_native` delegates to its pure native selectors
   (including child input restrictions), and `navigation` calls `registry::run`.
   This library neither embeds nor generates another public command list.
4. Call `Frontdoor::prepare(entry, args, Terminal::current(), adapter,
   &SourceInventory::embedded())` before any Python activation. Preparation
   discovers checkout ownership, selects the route, and validates a present
   native payload for navigation or migrated commands without starting a process.
5. Match `Prepared::route()`. For navigation call `Prepared::navigation`; no
   args on a noninteractive stream becomes `--help`, while two TTYs select the
   existing panel. Exact root help/list spelling and argument errors belong to
   the parent's registry. Public `.sh` aliases normalize before native selection.
6. For an in-process migrated command, call `Prepared::native_args` immediately
   before its existing implementation. This rechecks the process-local proof
   and selected executable identity without hashing payload bytes again. The
   parent should retain the preparation object until the action starts.
7. A separate authenticated launcher may instead consume
   `Prepared::into_native_exec().execute()`. It checks again immediately before
   exec and constructs only `--oms-dispatch ROOT REVISION -- ARGS...`, preserving
   raw argument bytes and caller cwd. It removes foreign Git location/config
   selectors from the child environment. The child must validate its private
   invocation; this alternative necessarily crosses another process boundary.
8. For existing private select/dispatch entrypoints use
   `ValidatedPayload::private(root, revision, inventory)` once, then retain the
   returned `Discovery` and `ValidatedPayload`. It checks current revision,
   content and executable identity. Do not call `load` again before dispatch.
9. Only a selected legacy route may call `Prepared::into_legacy`. Its checked
   script, raw local args, `PythonRequirement`, and `replace_process` preserve
   the old dispatcher policy: doctor/uninstall and any `-h`/`--help` do not
   require Python; panel/harness replace the process. The parent owns Bash 5
   selection, Python activation and the status-2 usage hint. A completed native
   action's status 75 is final, never a reason to retry through legacy code.
10. Write errors using `Error::diagnostic()` plus LF and exit with `Error::status`.
    `Display` is a lossy projection; diagnostics preserve non-UTF8 paths.
11. Construct `ReferenceProducer::new(&payload)` for Reference scope services.
    Its checked `protocol()` shares the payload's retained `Controller` and
    selects `Protocol::production_with_controller`, including `NativeIdentity`
    and `native_checked_append`. `controller()` shares that same snapshot with
    other native consumers. Construction grants no publication authority.

A missing cache or current-revision entry retains staged rollout fallback.
Corruption, unsafe authority or stale content in a present selected entry is a
terminal status 2. Failed COPY discovery retains the three-line status-1
checkout/receipt/reinstall diagnostic. Unknown/invalid tool names are status 2.
No runtime path writes or repairs an entry.

## Source and bootstrap checklist

- Checkout: resolve the dispatcher's logical authority root physically, verify
  protected entries without following descendant links, and require a regular
  marker: `scripts/agent-task.sh` (transitional), `crates/oms/Cargo.toml`, or
  `scripts/oms`. Launcher adjacency never accepts `scripts/oms` alone, since the
  launcher cannot vouch for itself. Explicit private roots and receipt roots
  accept all three.
- Symlink: resolve relative/absolute launcher targets for at most 40 links;
  reject loops, nonregular final entries, and protected scripts directory links.
  Physical root aliases, including macOS `/tmp` and `/var` aliases, are allowed.
- COPY: read `OMS_INSTALL_RECEIPT`, else the XDG config root, else the HOME config
  root. Parse bounded unique-key finite JSON in-process; require integer schema
  2 and an absolute `source_root`. Decode Python surrogateescape paths without
  UTF-8 replacement. Never execute helpers found through the receipt.
- Receipt boundary: the config root (or explicitly overridden receipt parent)
  is the selected authority anchor; `oh-my-setting/install.json` descendants
  are protected. Its open record remains bound to the discovery proof. Other
  install receipt fields confer no execution or lifecycle authority.
- Revision: read owned `.git`, `HEAD`, optional worktree `commondir`, symbolic
  loose refs or `packed-refs` directly. Bound reference chains at 32; validate
  lowercase 40-hex revisions and reference paths. Ambient Git variables do not
  affect discovery. SHA-256 repositories and reftable storage are unavailable
  under the existing SHA-1 payload contract; errors remain explicit.
- Payload: require fixed directories `target/oms-native/REVISION` and fixed
  regular leaves `oms`, `oms.sha256`, `manifest.json`, plus only the optional
  manifest-declared companions `oms-supervisor`, `oms-state-queue-worker`,
  `oms-mcp-peer-owner`, `oms-run-bounded-helper` and the transitional
  `oms-graph-python-helper`. Each listed companion must be a clean regular
  executable matching its digest; `ValidatedPayload::companion(name)` returns it.
  Unlisted or unknown directory content fails.
- Manifest: preserve schema 1, `source_commit` and fixed `artifacts` names.
  Require lowercase 64-hex digests. Preserve unknown top-level JSON fields;
  duplicate names, nonfinite numbers and malformed required fields fail.
- Checksum: require exactly 64 lowercase hex bytes plus LF, matching manifest
  `artifacts.oms`; hash the executable once before any exec. Hash each listed
  companion once too. The native binary never authenticates an unstarted verifier.
- Source content: the build follows local dependencies from `oms`, bootstrap,
  runtime, graph, peer and landing native roots plus the `oms-command-state` and
  `oms-mcp-server` producers of the state-queue and MCP companions. It embeds their `src` trees,
  manifests/build scripts, workspace manifest/lock and pinned toolchain, plus
  bundled `config`, `roles` and optional `prompts` trees. This includes
  `config/capability-profiles.json`, embedded SQL/help/data and source-local
  tests. Standalone dependency locks, targets, separate tests and docs are
  excluded. Local dependency paths use the repository's inline double-quoted
  `../CRATE` manifest form. Build watches cover those inputs and source/data
  subtrees, never the entire workspace or standalone build directories.
  Runtime indexes the reviewed manifest, enumerates each source/data directory
  once, and opens/hashes each source file once. Rechecks use shared retained
  parent descriptors, file metadata and directory generations, without another
  walk, open or hash. Missing optional data roots/build scripts are rechecked
  for additions. No runtime cache receipt substitutes for source bytes.
- Filesystem: retain nofollow directory/file handles; require a regular single
  link for every authority file; check executable bits, owner and write modes;
  reject set-ID files and setuid directories. Root or the effective user may
  own read authority; group writes and directory setgid are allowed, preserving
  shared/umask `002` installs. Other writes fail except root-owned sticky system
  ancestors. A writable group belongs to the installation's trust boundary.
  These ownership/world-write checks strengthen the Bash and existing native
  validators, which only require ordinary protected payload types and hashes.
  Recheck device/inode/mode/owner/group namespace identity and stable file
  size/timestamps. Reads use positional I/O and never share descriptor cursors.
- Loaded image: on Linux, prove the retained executable against the fixed
  kernel executable link under checked procfs descriptors. Create one Reference
  `Controller` from that descriptor and reuse its digest instead of hashing
  again. Named executable replacement cannot authenticate the loaded image.
  A separate launcher may validate and exec another image. In-process use and
  Reference service construction refuse when the image cannot be proven.
  On macOS, `Controller::capture_if_current` proves the retained file is the
  running executable through the Reference `F_GETPATH` provider (ancestor,
  owner and generation checks); a different file yields no controller and an
  executing file that fails the proof is a terminal error. Pathname/digest
  equality alone never substitutes for that proof.
- Limits: manifest/receipt 4 MiB, Git pointer/ref 64 KiB, packed refs 16 MiB,
  native source file 64 MiB, executable/helper 1 GiB, source closure 8192 files
  and 64 path components. Source snapshots also bound retained directories at
  8192. Empty/duplicate/malformed reviewed inventories fail explicitly.
- Producer: `ReferenceProducer` replaces the unused lifecycle request/receipt
  placeholders and unavailable executor with the actual Reference service.
  Payload staging, install receipt and frontdoor ownership transactions remain
  separate lifecycle-owner responsibilities; Reference has no such operations.

## Integrity and concurrent interruption

The trust chain is reviewed stager -> authenticated trampoline target -> native
receipt/source/payload validation -> typed route -> checked action/exec. Disk
checksums are integrity metadata, not signatures. A malicious same-user owner
can replace code and metadata together; no protection against that actor or the
final same-user check/exec race is claimed. Rechecks detect namespace and file
changes before action; no proof is serialized, cached or imported from env.

Discovery, selection, receipt reading and validation write no files, refs,
records or environment. Concurrent calls therefore have independent open
handles; a kill leaves no runtime residue. `CheckedExec` changes only its child
Git environment and replaces the caller process; it creates no receipt or
fallback record. `ReferenceProducer` construction and accessors write nothing;
independent calls share no persistent cache. Calls to its protocol use Reference's
canonical operations lock, validators and scope publication rules; interrupted
operations can leave incomplete scope/attempt state, never inferred success.
Build-time Cargo writes its standalone lock and ignored target output, including
`source_inventory.rs`; Cargo serializes its own build directory. An interrupted
build can leave a partial generated file and must be rebuilt before any owner
stages an artifact. Never publish a partial build.

## Evidence and deferred verification

Decisive existing consumers are `scripts/oms` (source lookup, pre-exec checksum,
private selector/dispatch, legacy preflight), `scripts/lib/platform.sh`,
`scripts/lib/install-contract.sh` (receipt path/schema and cache ownership),
`scripts/link.sh::link_target` (quoted COPY launcher), and
`crates/oms/src/main.rs::validate_payload`. Existing references are
`crates/oms/tests/native_entry_contract.rs` and the native COPY/cache repair
sections of `tests/install-lifecycle-smoke.sh`; these are unchanged.

The parent must integrate and test checkout, symlink, unrendered receipt COPY
and rendered COPY entrypoints with Python and managed runtime absent; verify
help/list/default TTY parity, final action 75, raw paths, stale source, all
malformed/empty/zero/equal-limit boundaries, protected links/hardlinks/FIFOs,
namespace replacement and pre-exec tampering. Independent security review,
Linux floor/macOS runtime checks, admission, deployment and lifecycle conversion
remain parent work. Compilation is not evidence that installed paths are
Python-free. This implementation phase deliberately adds/runs no tests, probes,
reversal rounds, CI, install or deployment.
