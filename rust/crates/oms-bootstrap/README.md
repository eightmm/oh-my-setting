# Native frontdoor preparation

This standalone library supplies the read-only bootstrap for the parent's OMS
binary. It has no public executable, competing command registry, runtime
interpreter, runtime Git invocation, download, build, or lifecycle fallback.
The workspace and lock are local to this crate; generated output is ignored
under `/target/`. All dependencies already exist in the repository.

## Parent integration contract

The parent owns `scripts/oms`, `rust/crates/oms/src/main.rs`, its `registry.rs`,
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

A missing cache or current-revision entry retains staged rollout fallback.
Corruption, unsafe authority or stale content in a present selected entry is a
terminal status 2. Failed COPY discovery retains the three-line status-1
checkout/receipt/reinstall diagnostic. Unknown/invalid tool names are status 2.
No runtime path writes or repairs an entry.

## Source and bootstrap checklist

- Checkout: resolve the dispatcher's logical authority root physically, verify
  protected scripts without following descendant links, and require the regular
  `scripts/agent-task.sh` marker. Explicit private roots use the same boundary.
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
- Payload: require fixed directories `rust/target/oms-native/REVISION` and fixed
  regular leaves `oms`, `oms.sha256`, `manifest.json`, plus only the optional
  manifest-declared `oms-graph-python-helper`. Unknown directory content fails.
- Manifest: preserve schema 1, `source_commit` and fixed `artifacts` names.
  Require lowercase 64-hex digests. Preserve unknown top-level JSON fields;
  duplicate names, nonfinite numbers and malformed required fields fail.
- Checksum: require exactly 64 lowercase hex bytes plus LF, matching manifest
  `artifacts.oms`; hash the executable once before any exec. Hash an optional
  helper once too. The native binary never authenticates an unstarted verifier.
- Source content: the build embeds a sorted raw-path/digest inventory of all
  crate `src` trees, crate manifests/locks/build scripts, workspace manifest/lock,
  and pinned toolchain. This includes embedded SQL/help/data and source-local
  unit tests. Separate test and documentation directories are excluded. Runtime
  enumerates that same closure, compares every path and digest, and detects
  additions/removals. No runtime cache receipt substitutes for those bytes.
- Filesystem: retain nofollow directory/file handles; require a regular single
  link for every authority file; check executable bits, owner and write modes;
  reject set-ID metadata. Root or the effective user may own read authority;
  group/other writes fail except root-owned sticky system ancestors. Recheck
  device/inode/mode/owner namespace identity and stable file size/timestamps.
- Limits: manifest/receipt 4 MiB, Git pointer/ref 64 KiB, packed refs 16 MiB,
  native source file 64 MiB, executable/helper 1 GiB, source closure 8192 files
  and 64 directory components. All overflow and malformed paths fail explicitly.
- Lifecycle: `ProducerRequest` names only staging a payload, writing an install
  receipt or materializing a frontdoor. `ProducerReceipt` is a typed in-memory
  acknowledgment binding operation/root/revision/destination/content digest;
  it introduces no persisted schema and grants no dispatch authority.
  `ProducerRequest::execute` always returns `ProducerUnavailable`. Real
  install/update/uninstall ownership transactions remain separate producers.

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
fallback record. Producer requests also write nothing. Build-time Cargo writes
its standalone lock and ignored target output; Cargo serializes its own build
directory. An interrupted build can leave ignored generated files and must be
rebuilt before any owner stages an artifact. Never publish a partial build.

## Evidence and deferred verification

Decisive existing consumers are `scripts/oms` (source lookup, pre-exec checksum,
private selector/dispatch, legacy preflight), `scripts/lib/platform.sh`,
`scripts/lib/install-contract.sh` (receipt path/schema and cache ownership),
`scripts/link.sh::link_target` (quoted COPY launcher), and
`rust/crates/oms/src/main.rs::validate_payload`. Existing references are
`rust/crates/oms/tests/native_entry_contract.rs` and the native COPY/cache repair
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
