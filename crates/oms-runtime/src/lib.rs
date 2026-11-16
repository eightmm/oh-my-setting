//! Shared runtime semantic API for CLI, panel and MCP consumers.
//!
//! Consumers call `parser::parse`, resolve execution/shared-state roots through
//! `Services::resolve_roots`, open `storage::Runtime`, and call `router::dispatch`.
//! `router::run` combines those steps and preserves output streams and exit status.
//! Producers supply canonical facts; runtime projection/evidence/context assembly
//! stays inside this crate. There is no interpreter or legacy command fallback.
//!
//! Public mode/source checklist (implementation coverage; admission pending):
//!
//! | Surface | Runtime implementation | Existing native reuse |
//! | --- | --- | --- |
//! | envelope show/write | projection, evidence, router | contract Markdown, work completion, safe storage |
//! | evidence show/unbound/matrix/bind/revoke | receipt strength, lineage, digests, bindings | work matrix/status precedence |
//! | next/plain/explain, brief/before | projection and router | work action inbox, return brief, renderer |
//! | failure classify/catalog | router | complete command-failure API |
//! | context targets/files/requires, phases/budgets/external/debt | actual pack assembly | safe file readers; typed native dependency producer |
//! | context-inspect manifest/compare/current | router/context verification | context, safe-fs, context-report |
//! | context-inspect injections/hash/session/timeline | router mode validation | context-source/session |
//! | profile list/current/show/check/apply/install-plan/install | profile expansion, readiness, planning, state | safe storage; typed installer |
//! | release status/resolve/apply/promote | channels, digests, policy | typed native revision/publication producers |
//! | capsule export/verify/import/diff | portable schema, scrub, integrity, advisory continuity | lossless JSON and safe storage |
//! | backend describe/check/run | capabilities, options, finite limits | typed readiness and full native supervisor |
//! | target add/remove/list/status/sync | strict local registry, same-path validation | safe storage; typed SSH transport |
//! | experiment template/validate/compare | router | experiment and command-experiment APIs |
//! | experiment register/show/run/launch/sweep/summarize/evaluate/invariants | contracts, linkage, budgets, analysis | experiment normalization, runs bootstrap, typed run ledger |
//! | benchmark show/snapshot/record/compare | measurements, model/cohort attribution | lossless JSON and safe storage |
//! | doctor/strict, version, help, shared option placement | parser/router | native domain readers |
//!
//! Producer contracts are in `services`: root resolution, immutable repository
//! view, canonical task/verified plan/failure projections over captured bytes,
//! lineage establishment, graph/AST dependency anchors, private-group account
//! proof, backend readiness/supervision, profile installation, release Git and
//! publication, SSH checkout transport, native run ledger, host/process identity,
//! and reconciled GPU usage. `Unavailable` refuses each required producer.
//! Named-user home expansion additionally needs read-only `account_home` metadata.
//! Direct API consumers set `Runtime::cwd` to the invocation directory; `run`
//! supplies it automatically. Guarded writes/appends require the held canonical
//! lock, including the capsule pointer lock spanning its wrapper publication.
//! Services must preserve real lifecycle/room/approval/lease authority and use
//! the existing native task/plan/operations/runs/journal/memory producers. They
//! must never infer execution, verification, isolation or publication success.
//!
//! State/concurrency/crash inventory:
//!
//! | Destination | Concurrent calls and interruption |
//! | --- | --- |
//! | envelope output; profile state | canonical file lock/CAS; readers see whole old/new bytes; a kill may leave a private temporary |
//! | evidence bindings/revocations | canonical stream lock; serialized same-inode append; short/torn or uncertain durable writes return an error |
//! | context bundle then manifest | individually locked durable writes; a kill between writes can leave an unreferenced bundle |
//! | capsule export | locked atomic JSON publication; identical repeat retains the same content identity |
//! | capsule import wrapper then LATEST | one pointer lock spans both writes; a kill can leave an unpointed wrapper; a repeat validates the wrapper |
//! | target registry | lock spans read/modify/replace; duplicate add and missing remove refuse |
//! | experiment contract | per-id lock/CAS; differing contract refuses; same contract may update registration time |
//! | experiment reservation/settlement stream | lock protects budget count/reserve; a kill leaves a held reservation until recorded usage or native stale-process proof |
//! | experiment run stream | locked append; a recorded run counts even if killed before settlement |
//! | invariant stream | final append follows service receipts; a kill during a pack leaves independent receipts without a fabricated pack result |
//! | benchmark outcome/effectiveness streams | serialized native append; uncertain writes require reconciliation before retry |
//! | backend logs/receipts; installer receipts; release manifest/refs; SSH checkout; run ledger/journal/memory | independent native producer owns locks, exact output, fsync, crash and process recovery |
//!
//! Runtime JSON uses the existing explicit-bound Python-string parser at 8 MiB;
//! native value conversions impose no aggregate reparse. Strict context manifests
//! retain their 4 MiB bound and duplicate/non-finite refusal. Runtime append checks
//! an existing stream of at most 32 MiB, accepts a row of at most 1 MiB including
//! LF, reads at most one prior EOF byte, and makes one O_APPEND write. It retains
//! file/parent descriptors, generation and namespace proofs, and refuses torn
//! tails. Append generations are captured under the stream lock; earlier reads
//! retain compare-and-swap authority only for replacements. Internal JSON writes
//! include their final LF within the 8 MiB read bound. This is a static I/O budget,
//! without a measured performance claim.
//!
//! Explicit envelope/template `--output`, capsule export `--output`, and context
//! `--bundle`/`--manifest` alone authorize the separate external atomic writer.
//! Defaults and every internal state path remain confined. Backend log/receipt
//! and profile installer receipt flags carry explicit paths to their independent
//! producers, which must enforce the same protected-publication requirements.
//! External outputs normalize lexical components before capturing macOS aliases
//! and resolving existing ancestors like Python atomic_write_bytes. The last
//! .oms directory and its descendants retain strict no-follow traversal, as do
//! protected paths. Linked immediate parents and final destinations refuse.
//! Explicit JSON outputs have no state-file size cap. Prior work snapshots and
//! acknowledgement envelopes share strict duplicate/non-finite JSON validation.
//! Existing ancestor permissions are never relaxed. Replacement failures can
//! leave only a private temporary or an explicitly uncertain published result;
//! append failures retain prior rows and may leave a torn row or empty new file.
//!
//! Remaining integration and compatibility work is explicit. No concrete
//! producer is wired here. Work reports use the decoded-value work-inspect APIs
//! without imposing an aggregate reparse bound on current runtime views. Default
//! context layers skip missing files and explicit safety/size refusals; genuine
//! I/O failures, generation changes and interruption propagate. Reused Runs
//! bootstrap computes approximate sample means; its private RNG needs a
//! parent-owned numerical correction. Local experiment means and sample spread use exact dyadic sums
//! and final nearest-even rounding. General argparse/help/error byte parity,
//! Unicode version details, numerical/repr corpora and the full frozen context
//! corpus still need independent comparison. Settlement I/O errors propagate
//! rather than hiding uncertain budget state. No acceptance follows from compile.
//! No source cache, persisted envelope or portable import supplies mutation
//! authority. Real process ownership, NFS crash behavior, private-group accounts,
//! Ubuntu/WSL2/macOS behavior and installed CLI/panel/MCP wiring remain parent
//! admission work. Tests and frozen fixtures are unchanged by this source phase.
#![forbid(unsafe_code)]
pub mod backend;
pub mod benchmark;
pub mod capsule;
mod cli_schema;
pub mod context;
pub mod evidence;
pub mod exec_snapshot;
pub mod experiment;
mod help;
mod io;
pub mod parser;
pub mod profiles;
pub mod projection;
pub mod release;
pub mod router;
pub mod services;
pub mod ssh_adapter;
mod statistics;
pub mod storage;
pub mod targets;
pub mod value;
pub use oms_command_plan::{Error, Result};
pub use parser::{Mode, Request, parse};
pub use router::{Response, dispatch, run};
pub use services::{Services, Unavailable};
pub const CORE_VERSION: &str = "0.7.0";
pub(crate) fn error(message: impl ToString) -> Error {
    Error {
        code: 2,
        message: message.to_string(),
    }
}
pub(crate) fn unavailable(name: &str) -> Error {
    error(format!(
        "native {name} service unavailable; integration pending"
    ))
}
pub(crate) fn require(ok: bool, message: impl ToString) -> Result<()> {
    if ok { Ok(()) } else { Err(error(message)) }
}
pub(crate) fn status(code: i32, message: impl ToString) -> Error {
    Error {
        code,
        message: message.to_string(),
    }
}
