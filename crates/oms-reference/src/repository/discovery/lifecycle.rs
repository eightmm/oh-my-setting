//! Read-only display of canonical and individually verified scratch ledgers.
//! Every source is projected independently. Reported states are ledger events.
use super::*;
pub use oms_command_operations::observation::ObservedAttempt;
use oms_command_operations::observation::{self, ProjectionIssue};

const TOTAL_BYTES: usize = 2 * 1024 * 1024;
const TOTAL_ROWS: usize = 8192;
const DISPLAY_ROWS: usize = 96;
const LEDGER: &str = ".oms/lifecycle/events.jsonl";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleScope {
    /// The consumer already owns its canonical projection and budgets.
    ScratchOnly,
    CanonicalAndScratch,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Canonical,
    Scratch,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptHead {
    Branch,
    Detached,
    Unknown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceStatus {
    Ready,
    Missing,
    Changed,
    Unavailable,
    Invalid,
    SourceLimit,
    BudgetLimit,
    Deadline,
}
#[derive(Debug)]
pub struct LifecycleSource {
    pub id: String,
    pub kind: SourceKind,
    /// Classification of the checked-out HEAD at observation. Its ref name is private.
    pub head: AttemptHead,
    /// Only a fixed, conventional branch label may be displayed. An arbitrary
    /// branch can contain a private goal, username or credential.
    pub branch_label: Option<&'static str>,
    pub status: SourceStatus,
}
#[derive(Debug)]
pub struct LifecycleObservation {
    pub attempts: Vec<ObservedAttempt>,
    pub sources: Vec<LifecycleSource>,
    pub discovery_issue: Option<DiscoveryIssue>,
    pub unverified_checkouts: usize,
    pub unexamined_checkouts: usize,
    /// Successfully projected attempts outside the most recent display window.
    pub omitted_attempts: usize,
}

fn head(git_dir: &Path) -> Option<(AttemptHead, Option<&'static str>, fs::Metadata)> {
    let (raw, info) = storage::regular(&git_dir.join("HEAD"), 4096, 1).ok()?;
    let value = raw.strip_suffix(b"\n")?;
    if let Some(branch) = value
        .strip_prefix(b"ref: refs/heads/")
        .filter(|b| !b.is_empty() && !b.iter().any(u8::is_ascii_control))
    {
        let label = ["main", "master", "trunk", "develop"]
            .into_iter()
            .find(|l| l.as_bytes() == branch);
        Some((AttemptHead::Branch, label, info))
    } else if matches!(value.len(), 40 | 64) && value.iter().all(u8::is_ascii_hexdigit) {
        Some((AttemptHead::Detached, None, info))
    } else {
        None
    }
}

/// Rank evidence for inspection only. Names from the registry cannot turn a
/// hint into an authorized source. Open each fixed directory below the pinned
/// managed root without following links and never open the ledger here.
pub(super) fn prioritize(
    candidates: &mut Vec<&PathBuf>,
    managed: &Path,
    binding: &Proof,
    scan: &Snapshot<'_>,
) -> Checked<()> {
    use rustix::fs::{self as rfs, AtFlags, FileType, Mode};
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let root: fs::File = rfs::open(managed, flags, Mode::empty())
        .map_err(|_| Changed)?
        .into();
    let info = root.metadata().map_err(|_| Changed)?;
    if !binding
        .dirs
        .iter()
        .any(|(entry, old)| entry == managed && storage::same(old, &info))
    {
        return Err(Changed);
    }
    let uid = rustix::process::getuid().as_raw();
    let hint = |path: &Path| -> Option<(bool, i128, i128)> {
        let parent = path.parent()?.file_name()?;
        let child = path.file_name()?;
        let mut current = root.try_clone().ok()?;
        for (depth, name) in [parent, child, OsStr::new(".oms"), OsStr::new("lifecycle")]
            .into_iter()
            .enumerate()
        {
            current = rfs::openat(&current, name, flags, Mode::empty())
                .ok()?
                .into();
            let info = current.metadata().ok()?;
            if info.uid() != uid
                || info.mode() & 0o002 != 0
                || (depth == 0 && info.mode() & 0o777 != 0o700)
            {
                return None;
            }
        }
        let stat = rfs::statat(&current, "events.jsonl", AtFlags::SYMLINK_NOFOLLOW).ok()?;
        (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
            && stat.st_nlink == 1
            && stat.st_uid == uid
            && stat.st_mode & 0o002 == 0)
            .then(|| {
                (
                    stat.st_size > 0 && stat.st_size as u64 <= observation::MAX_SOURCE_BYTES as u64,
                    stat.st_mtime.into(),
                    stat.st_mtime_nsec.into(),
                )
            })
    };
    let mut ranked = Vec::with_capacity(candidates.len());
    for path in candidates.drain(..) {
        scan.check()?;
        ranked.push((std::cmp::Reverse(hint(path)), path));
    }
    ranked.sort_unstable();
    candidates.extend(ranked.into_iter().map(|(_, path)| path));
    Ok(())
}

struct LedgerRead {
    path: PathBuf,
    namespace: storage::Namespace,
    before: Option<fs::Metadata>,
    attempts: Vec<ObservedAttempt>,
}

impl LedgerRead {
    fn finish(self) -> std::result::Result<Vec<ObservedAttempt>, SourceStatus> {
        self.namespace.check().map_err(|_| SourceStatus::Changed)?;
        match (self.before, fs::symlink_metadata(self.path)) {
            (Some(before), Ok(after)) if storage::generation(&before, &after) => Ok(self.attempts),
            (None, Err(e)) if e.kind() == ErrorKind::NotFound => Err(SourceStatus::Missing),
            _ => Err(SourceStatus::Changed),
        }
    }
}

struct Window<'a> {
    scan: Snapshot<'a>,
    bytes: usize,
    rows: usize,
}
impl Window<'_> {
    fn read(&mut self, repo: &Path, id: &str) -> std::result::Result<LedgerRead, SourceStatus> {
        use SourceStatus::*;
        let path = repo.join(LEDGER);
        let namespace = storage::Namespace::snapshot(repo, &path).map_err(|_| Changed)?;
        let before = match fs::symlink_metadata(&path) {
            Ok(info) => info,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Ok(LedgerRead {
                    path,
                    namespace,
                    before: None,
                    attempts: Vec::new(),
                });
            }
            Err(_) => return Err(Unavailable),
        };
        if !before.is_file()
            || before.nlink() != 1
            || before.uid() != rustix::process::getuid().as_raw()
            || before.mode() & 0o002 != 0
        {
            return Err(Changed);
        }
        let size = usize::try_from(before.size()).map_err(|_| SourceLimit)?;
        if size > observation::MAX_SOURCE_BYTES {
            return Err(SourceLimit);
        }
        // The descriptor reader may inspect one extra byte to detect growth,
        // even if this source subsequently fails generation validation.
        if size + 1 > self.bytes {
            return Err(BudgetLimit);
        }
        self.bytes -= size + 1;
        let raw = storage::read_repo(repo, Path::new(LEDGER), size, false).map_err(|_| Changed)?;
        let projected = observation::observed_attempts(
            &raw,
            id,
            &mut self.rows,
            self.scan.deadline,
            self.scan.interrupted,
        )
        .map_err(|e| match e {
            ProjectionIssue::Invalid => Invalid,
            ProjectionIssue::SourceLimit => SourceLimit,
            ProjectionIssue::BudgetLimit => BudgetLimit,
            ProjectionIssue::Interrupted | ProjectionIssue::Deadline => Deadline,
        })?;
        Ok(LedgerRead {
            path,
            namespace,
            before: Some(before),
            attempts: projected,
        })
    }

    fn observe(
        &mut self,
        report: &mut LifecycleObservation,
        repo: &Path,
        git_dir: &Path,
        id: &str,
        kind: SourceKind,
        verify: impl Fn() -> Checked<()>,
    ) -> Checked<()> {
        let mut source = LifecycleSource {
            id: id.to_owned(),
            kind,
            head: AttemptHead::Unknown,
            branch_label: None,
            status: SourceStatus::Deadline,
        };
        match self.scan.check() {
            Err(Interrupted) => return Err(Interrupted),
            Err(_) => {}
            Ok(()) if verify().is_err() => source.status = SourceStatus::Changed,
            Ok(()) => {
                let observed_head = head(git_dir);
                let projected = self.read(repo, id);
                if self.scan.interrupted.load(Ordering::Acquire) {
                    return Err(Interrupted);
                }
                if verify().is_err() {
                    source.status = SourceStatus::Changed;
                } else {
                    if let Some((kind, label, before)) = observed_head
                        && fs::symlink_metadata(git_dir.join("HEAD"))
                            .is_ok_and(|after| storage::generation(&before, &after))
                    {
                        source.head = kind;
                        source.branch_label = label;
                    }
                    match projected.and_then(LedgerRead::finish) {
                        Ok(mut attempts) => {
                            source.status = SourceStatus::Ready;
                            report.attempts.append(&mut attempts);
                        }
                        Err(status) => source.status = status,
                    }
                }
            }
        }
        report.sources.push(source);
        Ok(())
    }
}

impl Resolver {
    /// Read only registered source generations inside the selected canonical
    /// project's Room scope. No external path or ledger can be supplied.
    pub fn observe_managed_lifecycle(
        &self,
        shared: &Path,
        scope: LifecycleScope,
        interrupted: &AtomicBool,
    ) -> Checked<LifecycleObservation> {
        let mut window = Window {
            scan: Snapshot {
                deadline: Instant::now() + OBSERVATION_TIME,
                interrupted,
            },
            bytes: TOTAL_BYTES,
            rows: TOTAL_ROWS,
        };
        let (discovery, binding, git_dir) = self.discover_sources(
            shared,
            &window.scan,
            INSPECTIONS,
            CandidateOrder::RecentLedger,
        )?;
        let mut report = LifecycleObservation {
            attempts: Vec::new(),
            sources: Vec::new(),
            discovery_issue: discovery.incomplete,
            unverified_checkouts: discovery.unverified,
            unexamined_checkouts: discovery.remaining,
            omitted_attempts: 0,
        };
        if scope == LifecycleScope::CanonicalAndScratch {
            let info = fs::symlink_metadata(&git_dir).map_err(|_| Changed)?;
            let mut hash = Sha256::new();
            hash.update(b"oms-canonical-lifecycle-source-v1\0");
            hash.update(shared.as_os_str().as_bytes());
            hash.update(info.dev().to_be_bytes());
            hash.update(info.ino().to_be_bytes());
            let canonical = format!("{:x}", hash.finalize());
            window.observe(
                &mut report,
                shared,
                &git_dir,
                &canonical,
                SourceKind::Canonical,
                || binding.check(),
            )?;
        }
        for scratch in &discovery.checkouts {
            window.observe(
                &mut report,
                &scratch.checkout,
                &scratch.git_dir,
                scratch.id(),
                SourceKind::Scratch,
                || scratch.revalidate(),
            )?;
        }
        binding.check()?;
        if interrupted.load(Ordering::Acquire) {
            return Err(Interrupted);
        }
        // Live attempts precede finished ones so newer finished rows never push a
        // still-running attempt out of the display window.
        let finished = |a: &ObservedAttempt| {
            oms_command_operations::TERMINAL_STATES.contains(&a.state.as_str())
        };
        report.attempts.sort_unstable_by(|a, b| {
            finished(a)
                .cmp(&finished(b))
                .then(b.updated_at.cmp(&a.updated_at))
                .then(a.id.cmp(&b.id))
        });
        report.omitted_attempts = report.attempts.len().saturating_sub(DISPLAY_ROWS);
        report.attempts.truncate(DISPLAY_ROWS);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Fixture;
    use super::*;
    use crate::{V, obj};
    use oms_command_operations::{append_lifecycle, new_event};
    use std::io::Write;

    fn row(id: &str, tool: &str, at: &str) -> V {
        let mut event = new_event(
            id,
            1usize.into(),
            "attempt.created",
            obj([
                ("to_state", "queued".into()),
                ("tool", tool.into()),
                ("provider", "codex".into()),
                (
                    "refs",
                    obj([("transcript", "private-content-sentinel".into())]),
                ),
            ]),
        )
        .unwrap();
        event.set("ts", at);
        event
    }
    fn append(repo: &Path, event: V) {
        append_lifecycle(repo, event, &AtomicBool::new(false)).unwrap();
    }
    fn observe(f: &Fixture, repo: &Path) -> LifecycleObservation {
        f.resolver
            .observe_managed_lifecycle(
                repo,
                LifecycleScope::CanonicalAndScratch,
                &AtomicBool::new(false),
            )
            .unwrap()
    }
    #[test]
    fn independent_native_ledgers_keep_equal_ids_distinct_and_drop_private_fields() {
        let f = Fixture::new("lifecycle");
        let repo = f.repo("canonical");
        let scratch = f.scratch(&repo, "started");
        super::super::tests::git(&repo, &["branch", "-m", "trunk"]);
        let empty = observe(&f, &repo);
        assert!(
            empty.attempts.is_empty()
                && empty
                    .sources
                    .iter()
                    .all(|s| s.status == SourceStatus::Missing)
        );
        assert!(!repo.join(".oms").exists());
        assert!(!scratch.join(".oms").exists());
        let private_marker = concat!("gh", "p_", "aaaaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaaaa");
        let mut native = row(private_marker, private_marker, "2026-01-01T00:00:00Z");
        native.set("provider", private_marker);
        append(&repo, native);
        append(
            &scratch,
            row(private_marker, "peer-delegate", "2026-01-02T00:00:00Z"),
        );
        let mut next = new_event(
            private_marker,
            2usize.into(),
            "attempt.state_changed",
            obj([("to_state", "starting".into())]),
        )
        .unwrap();
        next.set("ts", "2026-01-03T00:00:00Z");
        append(&scratch, next);
        let before = fs::symlink_metadata(scratch.join(LEDGER)).unwrap();
        let report = observe(&f, &repo);
        assert!(storage::generation(
            &before,
            &fs::symlink_metadata(scratch.join(LEDGER)).unwrap()
        ));
        assert!(
            report
                .sources
                .iter()
                .all(|s| s.status == SourceStatus::Ready)
        );
        assert_eq!(report.sources[0].kind, SourceKind::Canonical);
        assert_eq!(report.sources[0].branch_label, Some("trunk"));
        assert_eq!(report.sources[1].branch_label, None);
        super::super::tests::git(&repo, &["branch", "-m", "private-branch-sentinel"]);
        let private_branch = observe(&f, &repo);
        assert_eq!(private_branch.sources[0].head, AttemptHead::Branch);
        assert_eq!(private_branch.sources[0].branch_label, None);
        assert!(!format!("{private_branch:?}").contains("private-branch-sentinel"));
        assert_eq!(
            (report.sources[0].head, report.sources[1].head),
            (AttemptHead::Branch, AttemptHead::Detached)
        );
        assert_eq!(report.attempts.len(), 2);
        let (first, second) = (&report.attempts[0], &report.attempts[1]);
        assert_ne!(first.id, second.id);
        assert_eq!(first.source_id, report.sources[1].id);
        assert_eq!(
            (
                first.state.as_str(),
                first.updated_at.as_str(),
                first.tool,
                first.provider
            ),
            (
                "starting",
                "2026-01-03T00:00:00Z",
                Some("peer-delegate"),
                Some("codex")
            )
        );
        assert_eq!(second.source_id, report.sources[0].id);
        let scoped = f
            .resolver
            .observe_managed_lifecycle(&repo, LifecycleScope::ScratchOnly, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(scoped.sources.len(), 1);
        assert_eq!(scoped.sources[0].kind, SourceKind::Scratch);
        assert_eq!(scoped.attempts.as_slice(), std::slice::from_ref(first));
        assert_eq!(
            (second.state.as_str(), second.tool, second.provider),
            ("queued", None, None)
        );
        let output = format!("{report:?}");
        for private in [
            private_marker,
            "private-content-sentinel",
            repo.to_str().unwrap(),
            scratch.to_str().unwrap(),
            "refs/heads/",
        ] {
            assert!(!output.contains(private));
        }
    }
    #[test]
    fn rejects_incomplete_redirected_oversized_and_replaced_without_hiding_other_sources() {
        let f = Fixture::new("ledger-integrity");
        let repo = f.repo("project");
        let a = f.scratch(&repo, "a");
        let b = f.scratch(&repo, "b");
        let c = f.scratch(&repo, "c");
        let d = f.scratch(&repo, "d");
        let redirected = f.scratch(&repo, "e");
        std::os::unix::fs::symlink(d.join(".oms"), redirected.join(".oms")).unwrap();
        for root in [&a, &b, &c, &d] {
            fs::create_dir_all(root.join(".oms/lifecycle")).unwrap();
        }
        fs::File::create(a.join(LEDGER))
            .unwrap()
            .set_len(observation::MAX_SOURCE_BYTES as u64 + 1)
            .unwrap();
        append(&b, row("torn", "panel-main", "2026-01-01T01:00:00Z"));
        fs::OpenOptions::new()
            .append(true)
            .open(b.join(LEDGER))
            .unwrap()
            .write_all(b"{")
            .unwrap();
        let private = repo.join("other-ledger");
        fs::write(&private, b"{}\n").unwrap();
        fs::hard_link(&private, c.join(LEDGER)).unwrap();
        append(&d, row("valid", "peer-review", "2026-02-01T00:00:00Z"));
        let report = observe(&f, &repo);
        assert_eq!(report.sources.len(), 6);
        for status in [
            SourceStatus::Missing,
            SourceStatus::SourceLimit,
            SourceStatus::Invalid,
            SourceStatus::Changed,
            SourceStatus::Ready,
        ] {
            let expected = if status == SourceStatus::Changed {
                2
            } else {
                1
            };
            assert_eq!(
                report.sources.iter().filter(|s| s.status == status).count(),
                expected
            );
        }
        assert_eq!(report.attempts.len(), 1);
        let interrupted = AtomicBool::new(false);
        let mut window = Window {
            scan: Snapshot {
                deadline: Instant::now() + OBSERVATION_TIME,
                interrupted: &interrupted,
            },
            bytes: TOTAL_BYTES,
            rows: TOTAL_ROWS,
        };
        let id = &report.attempts[0].source_id;
        let ledger = window.read(&d, id).unwrap();
        let replacement = d.join("replacement");
        fs::write(&replacement, fs::read(d.join(LEDGER)).unwrap()).unwrap();
        fs::rename(replacement, d.join(LEDGER)).unwrap();
        assert_eq!(ledger.finish().unwrap_err(), SourceStatus::Changed);
        window.bytes = 0;
        assert!(matches!(
            window.read(&d, id),
            Err(SourceStatus::BudgetLimit)
        ));
        interrupted.store(true, Ordering::Release);
        assert_eq!(
            f.resolver
                .observe_managed_lifecycle(&repo, LifecycleScope::CanonicalAndScratch, &interrupted)
                .unwrap_err(),
            Interrupted
        );
    }
    #[test]
    fn later_attempts_survive_the_combined_display_window() {
        let f = Fixture::new("display-window");
        let repo = f.repo("project");
        let wt = f.scratch(&repo, "one");
        fs::create_dir_all(repo.join(".oms/lifecycle")).unwrap();
        let mut out = fs::File::create(repo.join(LEDGER)).unwrap();
        for n in 0..=DISPLAY_ROWS {
            out.write_all(
                &row(
                    &format!("earlier-{n}"),
                    "panel-main",
                    "2025-01-01T00:00:00Z",
                )
                .json(false)
                .unwrap(),
            )
            .unwrap();
            out.write_all(b"\n").unwrap();
        }
        drop(out);
        append(&wt, row("later", "peer-review", "2026-01-01T00:00:00Z"));
        let report = observe(&f, &repo);
        assert_eq!(
            (report.attempts.len(), report.omitted_attempts),
            (DISPLAY_ROWS, 2)
        );
        assert_eq!(report.attempts[0].source_id, report.sources[1].id);
    }
    #[test]
    fn older_live_attempt_survives_newer_finished_ones() {
        let f = Fixture::new("display-live");
        let repo = f.repo("project");
        let wt = f.scratch(&repo, "one");
        fs::create_dir_all(repo.join(".oms/lifecycle")).unwrap();
        let mut out = fs::File::create(repo.join(LEDGER)).unwrap();
        for n in 0..=DISPLAY_ROWS {
            let id = format!("finished-{n}");
            let mut done = new_event(
                &id,
                2usize.into(),
                "attempt.state_changed",
                obj([
                    ("from_state", "queued".into()),
                    ("to_state", "cancelled".into()),
                ]),
            )
            .unwrap();
            done.set("ts", "2026-01-01T00:00:01Z");
            for event in [row(&id, "peer-review", "2026-01-01T00:00:00Z"), done] {
                out.write_all(&event.json(false).unwrap()).unwrap();
                out.write_all(b"\n").unwrap();
            }
        }
        drop(out);
        append(&wt, row("live", "peer-delegate", "2025-01-01T00:00:00Z"));
        let report = observe(&f, &repo);
        assert_eq!(report.attempts.len(), DISPLAY_ROWS);
        assert_eq!(report.attempts[0].state, "queued");
        assert_eq!(report.attempts[0].source_id, report.sources[1].id);
    }
    #[test]
    fn usable_ledger_outranks_newer_empty_or_oversized_sources_and_still_requires_proof() {
        let f = Fixture::new("coverage");
        let repo = f.repo("project");
        for i in 0..INSPECTIONS {
            let checkout = f.scratch(&repo, &format!("a{i:03}"));
            fs::create_dir_all(checkout.join(".oms/lifecycle")).unwrap();
            let unsuitable = fs::File::create(checkout.join(LEDGER)).unwrap();
            if i % 2 == 0 {
                unsuitable
                    .set_len(observation::MAX_SOURCE_BYTES as u64 + 1)
                    .unwrap();
            }
            unsuitable
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(120))
                .unwrap();
        }
        let recent = f.scratch(&repo, "zz-recent");
        append(
            &recent,
            row("new-worker", "peer-delegate", "2026-01-01T12:00:00Z"),
        );
        fs::File::options()
            .write(true)
            .open(recent.join(LEDGER))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(60))
            .unwrap();
        let original = f
            .resolver
            .discover_scratch_checkouts(&repo, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(original.remaining, 1);
        assert!(
            original
                .checkouts
                .iter()
                .all(|item| item.checkout != recent)
        );
        let report = observe(&f, &repo);
        assert_eq!(
            (
                report.sources.len(),
                report.unexamined_checkouts,
                report.discovery_issue
            ),
            (INSPECTIONS + 1, 1, Some(InspectionLimit))
        );
        assert_eq!(report.attempts.len(), 1);
        assert_eq!(report.attempts[0].source_id, report.sources[1].id);
        assert!(
            report
                .sources
                .iter()
                .any(|s| s.status == SourceStatus::SourceLimit)
        );
        assert!(
            report
                .sources
                .iter()
                .filter(|s| s.status == SourceStatus::Ready)
                .count()
                > 1
        );
        let marker = recent.parent().unwrap().join(".oh-my-setting-tmp");
        fs::write(marker, b"kind=unverified\n").unwrap();
        let forged = observe(&f, &repo);
        assert!(forged.attempts.is_empty());
        assert_eq!(forged.unverified_checkouts, 1);
    }
    #[test]
    fn absent_managed_root_keeps_verified_canonical_work_and_fails_closed_on_redirect_or_foreign_room()
     {
        let mut f = Fixture::new("first-install");
        let repo = f.repo("project");
        let other = f.repo("other");
        append(
            &repo,
            row("canonical", "panel-main", "2026-01-01T00:00:00Z"),
        );
        let managed = f.resolver.managed_root.clone();
        let prefix = managed.join("never-created");
        f.resolver.managed_root = prefix.join("worktrees");
        f.resolver.inherited_room = Some(repo.clone());
        let report = observe(&f, &repo);
        assert_eq!(
            (
                report.sources.len(),
                report.sources[0].kind,
                report.sources[0].status
            ),
            (1, SourceKind::Canonical, SourceStatus::Ready)
        );
        assert_eq!(
            (
                report.attempts.len(),
                report.unverified_checkouts,
                report.discovery_issue
            ),
            (1, 0, None)
        );
        let scratch = f
            .resolver
            .observe_managed_lifecycle(&repo, LifecycleScope::ScratchOnly, &AtomicBool::new(false))
            .unwrap();
        assert!(scratch.sources.is_empty() && scratch.discovery_issue.is_none());
        assert!(!prefix.exists());
        f.resolver.inherited_room = Some(other.clone());
        assert_eq!(
            f.resolver
                .observe_managed_lifecycle(
                    &repo,
                    LifecycleScope::CanonicalAndScratch,
                    &AtomicBool::new(false)
                )
                .unwrap_err(),
            OutsideScope
        );
        f.resolver.inherited_room = None;
        let interrupted = AtomicBool::new(false);
        let scan = Snapshot {
            deadline: Instant::now() + OBSERVATION_TIME,
            interrupted: &interrupted,
        };
        let (_, absent_binding, _) = f
            .resolver
            .discover_sources(&repo, &scan, INSPECTIONS, CandidateOrder::RecentLedger)
            .unwrap();
        std::os::unix::fs::symlink(other.join("dangling"), &prefix).unwrap();
        assert_eq!(absent_binding.check(), Err(Changed));
        assert!(
            f.resolver
                .observe_managed_lifecycle(
                    &repo,
                    LifecycleScope::CanonicalAndScratch,
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        fs::remove_file(&prefix).unwrap();
        std::os::unix::fs::symlink(&other, &prefix).unwrap();
        assert!(
            f.resolver
                .observe_managed_lifecycle(
                    &repo,
                    LifecycleScope::CanonicalAndScratch,
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        fs::remove_file(&prefix).unwrap();
        f.resolver.managed_root = managed.clone();
        let _removed = f.scratch(&repo, "registered");
        fs::rename(&managed, managed.with_extension("moved")).unwrap();
        let incomplete = observe(&f, &repo);
        assert_eq!(
            (
                incomplete.sources.len(),
                incomplete.attempts.len(),
                incomplete.unverified_checkouts,
                incomplete.discovery_issue
            ),
            (1, 1, 1, Some(UnverifiedRegistration))
        );
    }
}
