use crate::model::*;
use crate::storage::{Snapshot, Store, stable};
use crate::value::{Value, array, encode, obj, strings};
use crate::{
    App, DeliveryRequest, Error, Result, Services, Session, checkpoint, digest, now, unique,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub struct Room<'a> {
    pub(crate) repo: PathBuf,
    pub(crate) store: Option<Store>,
    pub(crate) session: Session,
    pub(crate) services: &'a dyn Services,
    pub(crate) stop: &'a AtomicBool,
    snapshots: crate::Shared<HashMap<String, Arc<Snapshot>>>,
    pub(crate) notices: crate::Shared<Vec<String>>,
}
#[derive(Clone, Debug)]
pub struct Join {
    pub participant: String,
    pub provider: String,
    pub role: String,
    pub model: Option<String>,
    pub label: Option<String>,
    pub native_session: Option<String>,
    pub owns: Vec<String>,
    pub parent: Option<String>,
    pub dispatch_role: Option<String>,
    pub purpose: Option<String>,
    pub state: Option<String>,
    pub alias: Option<String>,
    pub task_id: Option<String>,
}
impl Join {
    pub fn new(who: &str, provider: &str) -> Self {
        Self {
            participant: who.to_owned(),
            provider: provider.to_owned(),
            role: "main".to_owned(),
            model: None,
            label: None,
            native_session: None,
            owns: vec![],
            parent: None,
            dispatch_role: None,
            purpose: None,
            state: None,
            alias: None,
            task_id: None,
        }
    }
}
pub struct Send<'a> {
    pub sender: &'a str,
    pub recipient: &'a str,
    pub text: &'a str,
    pub kind: &'a str,
    pub reply_to: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub linked: bool,
    pub quiet: bool,
}
impl<'a> Send<'a> {
    pub fn note(sender: &'a str, recipient: &'a str, text: &'a str) -> Self {
        Self {
            sender,
            recipient,
            text,
            kind: "note",
            reply_to: None,
            message_id: None,
            linked: true,
            quiet: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    None,
    Ambiguous,
    Bound { room: String, participant: String },
}
#[derive(Clone, Debug)]
pub struct LineageEntry {
    pub id: String,
    pub calls: Vec<Value>,
    pub publications: Value,
    pub call_seqs: Value,
}
#[derive(Clone, Copy)]
pub(crate) enum Carry<'a> {
    No,
    Replay(Option<&'a str>),
}
impl Carry<'_> {
    pub fn active(self) -> bool {
        matches!(self, Self::Replay(_))
    }
}
impl<'a> Room<'a> {
    pub fn open(
        repo: &Path,
        session: Session,
        services: &'a dyn Services,
        stop: &'a AtomicBool,
        create: bool,
        allow_foreign: bool,
    ) -> Result<Self> {
        checkpoint(stop)?;
        if create && session.child {
            return Err(Error::new("a worker cannot create a room"));
        }
        let repo = services.state_repository(repo)?;
        let repo = std::fs::canonicalize(repo).map_err(|_| Error::new("repository unavailable"))?;
        if !allow_foreign
            && session.room.is_some()
            && let Some(inherited) = &session.repository
        {
            let inherited = std::fs::canonicalize(services.state_repository(inherited)?)
                .map_err(|_| Error::new("inherited room repository unavailable"))?;
            if inherited != repo {
                return Err(Error::new(
                    "selected repository conflicts with the inherited room repository",
                ));
            }
        }
        let threads_exist = match std::fs::symlink_metadata(repo.join(".oms/threads")) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(Error::new("room directory unavailable")),
        };
        let store = if create || threads_exist {
            Some(Store::open(&repo, create)?)
        } else {
            None
        };
        Ok(Self {
            repo,
            store,
            session,
            services,
            stop,
            snapshots: crate::Shared::new(HashMap::new()),
            notices: crate::Shared::new(Vec::new()),
        })
    }
    pub fn repository(&self) -> &Path {
        &self.repo
    }
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.borrow_mut())
    }
    pub(crate) fn storage(&self) -> Result<&Store> {
        self.store
            .as_ref()
            .ok_or_else(|| Error::new("room repository has no thread logs"))
    }
    pub(crate) fn snapshot(&self, id: &str) -> Result<Arc<Snapshot>> {
        identifier(id, "room")?;
        checkpoint(self.stop)?;
        let store = self.storage()?;
        store.check()?;
        let cached = self.snapshots.borrow().get(id).cloned();
        if let Some(old) = cached {
            let mut reader = crate::storage::LogReader::open(&store.threads, Store::name(id))?;
            if let Some(snapshot) = reader.refresh(&old)? {
                let snapshot = Arc::new(snapshot);
                self.snapshots
                    .borrow_mut()
                    .insert(id.to_owned(), Arc::clone(&snapshot));
                return Ok(snapshot);
            }
            return Ok(old);
        }
        let snapshot = Arc::new(store.snapshot(id)?);
        self.snapshots
            .borrow_mut()
            .insert(id.to_owned(), Arc::clone(&snapshot));
        Ok(snapshot)
    }
    pub fn records(&self, id: &str) -> Result<Vec<Value>> {
        Ok(self.snapshot(id)?.rows.clone())
    }
    pub(crate) fn invalidate(&self, id: &str) {
        self.snapshots.borrow_mut().remove(id);
    }
    pub(crate) fn header(&self, id: &str) -> Result<Vec<Value>> {
        self.header_bounded(id, 2 * crate::MAX_ROW)
            .map(|(rows, _)| rows)
    }
    fn header_bounded(&self, id: &str, budget: usize) -> Result<(Vec<Value>, usize)> {
        identifier(id, "room")?;
        if budget == 0 {
            return Err(Error::new("room discovery byte limit"));
        }
        if let Some(snapshot) = self.snapshots.borrow().get(id) {
            let store = self.storage()?;
            store.check()?;
            let stat = rustix::fs::statat(
                &store.threads.fd,
                Store::name(id),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| Error::new("room log unavailable"))?;
            if stable(&snapshot.stat, &stat) {
                let used = snapshot
                    .ranges
                    .iter()
                    .take(2)
                    .next_back()
                    .map(|r| r.end)
                    .unwrap_or(0);
                if used > budget {
                    return Err(Error::new("room discovery byte limit"));
                }
                return Ok((snapshot.rows.iter().take(2).cloned().collect(), used));
            }
        }
        let store = self.storage()?;
        store.check()?;
        let (first, _, _) =
            store
                .threads
                .prefix_tail(&Store::name(id), budget.min(2 * crate::MAX_ROW), 0)?;
        store.check()?;
        let mut out = Vec::new();
        for line in first.split_inclusive(|b| *b == b'\n').take(2) {
            if !line.ends_with(b"\n") || line.len() > crate::MAX_ROW {
                return Err(Error::new("room header is oversized or incomplete"));
            }
            let r = crate::value::parse(line, crate::MAX_ROW)?;
            if r.as_object().is_none()
                || r.text("thread") != id
                || r["seq"].as_i64().is_none()
                || r.get("room_event").is_some_and(|e| e.as_object().is_none())
            {
                return Err(Error::new("invalid room header"));
            }
            out.push(r);
        }
        Ok((out, first.len()))
    }
    pub(crate) fn predecessor(&self, id: &str) -> Result<Option<String>> {
        let rows = self.header(id)?;
        Ok(rows
            .get(1)
            .filter(|r| r["room_event"].text("kind") == "created")
            .and_then(|r| r["room_event"]["predecessor"].as_str())
            .map(str::to_owned))
    }
    pub(crate) fn tail_successor(&self, id: &str) -> Result<Option<String>> {
        identifier(id, "room")?;
        let store = self.storage()?;
        store.check()?;
        let (_, tail, stat) = store.threads.prefix_tail(&Store::name(id), 0, TAIL_BYTES)?;
        store.check()?;
        if tail.is_empty() || !tail.ends_with(b"\n") {
            return Ok(None);
        }
        let start = tail[..tail.len() - 1]
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        if start == 0 && stat.st_size as usize > tail.len() {
            return Ok(None);
        }
        let row = match crate::value::parse(&tail[start..], TAIL_BYTES) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let e = &row["room_event"];
        if row.text("thread") == id
            && e.text("kind") == "successor"
            && identifier(e.text("target"), "room").is_ok()
        {
            Ok(Some(e.text("target").to_owned()))
        } else {
            Ok(None)
        }
    }
    pub fn resolve(&self, id: &str) -> Result<String> {
        identifier(id, "room")?;
        let mut current = id.to_owned();
        let mut seen = BTreeSet::from([current.clone()]);
        for _ in 0..MAX_HOPS {
            checkpoint(self.stop)?;
            let next = match self.tail_successor(&current) {
                Ok(Some(n)) => n,
                _ => return Ok(current),
            };
            if seen.contains(&next)
                || self.predecessor(&next).ok().flatten().as_deref() != Some(current.as_str())
            {
                return Ok(current);
            }
            seen.insert(next.clone());
            current = next;
        }
        Err(Error::new("room forwarding chain too long"))
    }
    pub fn same_room(&self, a: &str, b: &str) -> Result<bool> {
        Ok(a == b || !a.is_empty() && !b.is_empty() && self.resolve(a)? == self.resolve(b)?)
    }
    pub fn sealed_predecessors(&self, id: &str) -> Result<Vec<String>> {
        self.predecessors(id, true)
    }
    pub(crate) fn bound(&self, id: &str) -> Result<bool> {
        self.session
            .room
            .as_deref()
            .map(|r| self.same_room(r, id))
            .unwrap_or(Ok(false))
    }
    pub(crate) fn predecessors(&self, id: &str, sealed: bool) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut current = id.to_owned();
        let mut seen = BTreeSet::from([current.clone()]);
        for _ in 0..MAX_HOPS {
            let Some(old) = self.predecessor(&current)? else {
                return Ok(out);
            };
            if seen.contains(&old) {
                if sealed {
                    return Err(Error::new("structured result lineage is not sealed"));
                }
                return Ok(out);
            }
            if sealed {
                match self.tail_successor(&old)? {
                    None => {
                        if project(&self.snapshot(&current)?.rows)?.flag("carried") {
                            return Err(Error::new("structured result lineage is unreadable"));
                        }
                        return Ok(out);
                    }
                    Some(next)
                        if next == current && crate::carry::successor_id(&old) == current => {}
                    _ => return Err(Error::new("structured result lineage is not sealed")),
                }
            }
            seen.insert(old.clone());
            out.push(old.clone());
            current = old;
        }
        if self.predecessor(&current)?.is_some() {
            Err(Error::new("structured result lineage is too long"))
        } else {
            Ok(out)
        }
    }
    pub(crate) fn structured_rows(&self, id: &str, rows: &[Value]) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for old in self.predecessors(id, true)?.iter().rev() {
            out.extend(structured_of(&self.snapshot(old)?.rows)?);
        }
        out.extend(structured_of(rows)?);
        Ok(out)
    }
    pub fn structured_events(&self, id: &str) -> Result<Vec<Value>> {
        let id = self.resolve(id)?;
        let snap = self.snapshot(&id)?;
        Ok(self
            .structured_rows(&id, &snap.rows)?
            .into_iter()
            .map(|r| r["room_event"].clone())
            .collect())
    }
    pub fn result_participant(&self, state: &Value, who: &str) -> Result<Value> {
        if state
            .list("participants")
            .iter()
            .any(|p| p.text("participant") == who)
        {
            return Ok(member(state, who)?.clone());
        }
        for old in self.predecessors(state.text("id"), true)? {
            let before = project(&self.snapshot(&old)?.rows)?;
            if before
                .list("participants")
                .iter()
                .any(|p| p.text("participant") == who)
            {
                return Ok(member(&before, who)?.clone());
            }
        }
        Ok(member(state, who)?.clone())
    }
    pub(crate) fn lineage_messages(
        &self,
        state: &Value,
        mids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Value>> {
        let mut wanted = mids.clone();
        let mut out = BTreeMap::new();
        for old in self.predecessors(state.text("id"), true)? {
            if wanted.is_empty() {
                break;
            }
            for m in project(&self.snapshot(&old)?.rows)?.list("messages") {
                if wanted.remove(m.text("id")) {
                    out.insert(m.text("id").to_owned(), m.clone());
                }
            }
        }
        Ok(out)
    }
    pub(crate) fn append_at(
        &self,
        id: &str,
        event: &Value,
        text: &str,
        carry: Carry<'_>,
    ) -> Result<bool> {
        Ok(self.append_batch_at(id, &[(event, text, carry)])?[0])
    }
    pub(crate) fn append_batch_at(
        &self,
        id: &str,
        writes: &[(&Value, &str, Carry<'_>)],
    ) -> Result<Vec<bool>> {
        identifier(id, "room")?;
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        checkpoint(self.stop)?;
        let store = self.storage()?;
        let guard = store.lock(id, self.stop)?;
        guard.check().map_err(|e| Error::new(e.to_string()))?;
        let snapshot = self.snapshot(id)?;
        let mut rows = snapshot.rows.clone();
        let mut ranges = snapshot.ranges.clone();
        let mut bytes = Vec::new();
        let mut applied = Vec::with_capacity(writes.len());
        let mut proofs = Vec::new();
        let mut seq = rows
            .iter()
            .filter_map(|r| r["seq"].as_i64())
            .max()
            .unwrap_or(0);
        for &(event, original, carry) in writes {
            checkpoint(self.stop)?;
            if original.is_empty() {
                return Err(Error::new("--text or --text-file is required"));
            }
            check_turn_text(original)?;
            let text = turn_text(original);
            let text = text.as_str();
            if !self.validate_event_locked(
                &rows,
                event,
                text,
                carry,
                &mut proofs,
                text == original,
            )? {
                applied.push(false);
                continue;
            }
            let stamp = match carry {
                Carry::Replay(Some(t)) => {
                    if t.len() != 20
                        || t.bytes().enumerate().any(|(i, b)| match i {
                            4 | 7 => b != b'-',
                            10 => b != b'T',
                            13 | 16 => b != b':',
                            19 => b != b'Z',
                            _ => !b.is_ascii_digit(),
                        })
                        || chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%SZ").is_err()
                        || t > now().as_str()
                    {
                        return Err(Error::new("invalid carried room time"));
                    }
                    t.to_owned()
                }
                _ => now(),
            };
            seq = seq
                .checked_add(1)
                .ok_or_else(|| Error::new("thread sequence exhausted"))?;
            let row = obj([
                ("schema", 1u64.into()),
                ("ts", stamp.into()),
                ("thread", id.into()),
                ("seq", seq.into()),
                ("role", "note".into()),
                ("agent", self.session.agent.clone().into()),
                ("text", text.into()),
                ("room_event", event.clone()),
            ]);
            let mut line = encode(&row, false, false)?;
            line.push(b'\n');
            if line.len() > crate::MAX_ROW
                || snapshot.bytes.len() + bytes.len() + line.len() > crate::MAX_FILE
            {
                return Err(Error::new("thread storage budget exceeded"));
            }
            let start = snapshot.bytes.len() + bytes.len();
            bytes.extend_from_slice(&line);
            ranges.push(start..snapshot.bytes.len() + bytes.len());
            rows.push(row);
            applied.push(true);
        }
        if bytes.is_empty() {
            return Ok(applied);
        }
        checkpoint(self.stop)?;
        guard.check().map_err(|e| Error::new(e.to_string()))?;
        store.check()?;
        for proof in &proofs {
            proof.check()?;
        }
        let stat = store
            .threads
            .append(&Store::name(id), &snapshot.stat, &bytes)?;
        for proof in &proofs {
            proof.check().map_err(|e| {
                Error::new(format!(
                    "room append proof changed; publication is uncertain: {e}"
                ))
            })?;
        }
        let mut full = snapshot.bytes.clone();
        full.extend_from_slice(&bytes);
        self.snapshots.borrow_mut().insert(
            id.to_owned(),
            Arc::new(Snapshot {
                id: id.to_owned(),
                bytes: full,
                rows,
                ranges,
                stat,
            }),
        );
        guard.release().map_err(|e| Error::new(e.to_string()))?;
        Ok(applied)
    }
    pub fn append(&self, id: &str, event: &Value, text: &str) -> Result<(bool, String)> {
        let target = self.resolve(id)?;
        match self.append_at(&target, event, text, Carry::No) {
            Ok(v) => Ok((v, target)),
            Err(e)
                if e.message.contains("room continues as")
                    || e.message.contains("room carry is unfinished") =>
            {
                let target = self.settle(id)?;
                Ok((self.append_at(&target, event, text, Carry::No)?, target))
            }
            Err(e) => Err(e),
        }
    }
    pub fn create(
        &self,
        id: Option<&str>,
        title: &str,
        task: Option<&str>,
        creator: Option<&str>,
        predecessor: Option<&str>,
    ) -> Result<String> {
        if self.session.child {
            return Err(Error::new("a worker cannot create a room"));
        }
        let generated = format!("room-{}", &unique()?[..12]);
        let id = identifier(id.unwrap_or(&generated), "room")?;
        if title.trim().is_empty() || title.len() > 1000 {
            return Err(Error::new("room title needs 1..1000 bytes"));
        }
        let event = Self::created_event(task, creator, predecessor)?;
        check_turn_text(title)?;
        let stored_title = turn_text(title);
        let header = obj([
            ("schema", 1u64.into()),
            ("ts", now().into()),
            ("thread", id.into()),
            ("seq", 1u64.into()),
            ("role", "topic".into()),
            ("agent", self.session.agent.clone().into()),
            ("text", stored_title.clone().into()),
            ("live", true.into()),
        ]);
        self.validate_event_locked(
            std::slice::from_ref(&header),
            &event,
            title,
            Carry::No,
            &mut Vec::new(),
            false,
        )?;
        let row = obj([
            ("schema", 1u64.into()),
            ("ts", now().into()),
            ("thread", id.into()),
            ("seq", 2u64.into()),
            ("role", "note".into()),
            ("agent", self.session.agent.clone().into()),
            ("text", stored_title.into()),
            ("room_event", event),
        ]);
        let mut bytes = encode(&header, false, false)?;
        bytes.push(b'\n');
        bytes.extend(encode(&row, false, false)?);
        bytes.push(b'\n');
        let store = self.storage()?;
        let guard = store.lock(id, self.stop)?;
        guard.check().map_err(|e| Error::new(e.to_string()))?;
        store.check()?;
        if store.threads.exists(&Store::name(id))? {
            return Err(Error::new("room id already exists; join it explicitly"));
        }
        checkpoint(self.stop)?;
        store.threads.create(&Store::name(id), &bytes)?;
        guard.release().map_err(|e| Error::new(e.to_string()))?;
        self.invalidate(id);
        self.write_current(id)?;
        Ok(id.to_owned())
    }
    pub(crate) fn created_event(
        task: Option<&str>,
        creator: Option<&str>,
        predecessor: Option<&str>,
    ) -> Result<Value> {
        let mut e = obj([("kind", "created".into())]);
        for (k, v) in [
            ("task_id", task),
            ("participant", creator),
            ("predecessor", predecessor),
        ] {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                identifier(v, k)?;
                e.set(k, v);
            }
        }
        Ok(e)
    }
    fn write_current(&self, id: &str) -> Result<()> {
        let root = oms_safe_fs::Root::open(&self.repo)
            .map_err(|e| Error::new(format!("room pointer root: {e:?}")))?;
        let task = root
            .read_no_follow(
                Path::new(".oms/task/current.md"),
                oms_safe_fs::ByteReadOptions {
                    missing_ok: true,
                    max_bytes: 1024 * 1024,
                },
            )
            .map_err(|_| Error::new("room task metadata unavailable"))?;
        let mut task_id = String::new();
        for line in String::from_utf8_lossy(&task).lines() {
            if line.starts_with("## ") {
                break;
            }
            if let Some(value) = line.strip_prefix("- task_id:") {
                task_id = value.trim_start().to_owned();
                break;
            }
        }
        let bytes = format!("{id} {} {task_id}\n", crate::unix_seconds()).into_bytes();
        let path = self.repo.join(".oms/threads/CURRENT");
        let lock = oms_lock::Lock::for_file(
            &path,
            oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?,
        )
        .map_err(|e| Error::new(e.to_string()))?;
        let guard = Store::acquire(&lock, self.stop)?;
        checkpoint(self.stop)?;
        root.atomic_mutate(
            Path::new(".oms/threads/CURRENT"),
            oms_safe_fs::MutationOptions {
                read: oms_safe_fs::ByteReadOptions {
                    missing_ok: true,
                    max_bytes: 65536,
                },
                max_output: 65536,
                ..Default::default()
            },
            |_| Ok::<_, Error>(bytes),
        )
        .map_err(|e| Error::new(format!("room current pointer publication: {e:?}")))?;
        guard.release().map_err(|e| Error::new(e.to_string()))
    }
    pub fn join(&self, id: &str, join: Join) -> Result<Value> {
        let provider = self.services.canonical_provider(&join.provider)?;
        let mut session = join.native_session;
        if session.as_deref() == Some("current") {
            if provider != "claude" {
                return Err(Error::new(
                    "current shortcut is for the calling Claude session",
                ));
            }
            session = Some(self.services.current_claude_session()?);
        }
        let label = join
            .label
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| join.participant.clone());
        let mut e = obj([
            ("kind", "join".into()),
            ("participant", join.participant.into()),
            ("provider", provider.into()),
            ("role", join.role.into()),
            ("model", join.model.into()),
            ("label", label.clone().into()),
            ("owns", join.owns.into()),
        ]);
        for (k, v) in [
            ("parent", join.parent),
            ("dispatch_role", join.dispatch_role),
            ("purpose", join.purpose),
            ("state", join.state),
            ("alias", join.alias),
            ("task_id", join.task_id),
        ] {
            if let Some(v) = v {
                e.set(k, v);
            }
        }
        if let Some(s) = session.filter(|s| !s.is_empty()) {
            e.set("consumer", &digest(s.as_bytes())[..32]);
        }
        self.append(id, &e, &format!("Joined: {label}"))?;
        Ok(e)
    }
    pub fn bind(&self, id: &str, who: &str, native: &str, replaces: Option<&str>) -> Result<Value> {
        let native = if native == "current" {
            if member(&self.status(id, false)?, who)?.text("provider") != "claude" {
                return Err(Error::new(
                    "current shortcut is for the calling Claude session",
                ));
            }
            self.services.current_claude_session()?
        } else {
            native.to_owned()
        };
        identifier(&native, "native session")?;
        let mut e = obj([
            ("kind", "bind".into()),
            ("participant", who.into()),
            ("consumer", digest(native.as_bytes())[..32].into()),
        ]);
        if let Some(v) = replaces {
            e.set("previous", v);
        }
        self.append(
            id,
            &e,
            if replaces.is_some() {
                "Native session rebound"
            } else {
                "Native session bound"
            },
        )?;
        Ok(e)
    }
    pub fn scope(&self, id: &str, who: &str, scopes: &[String]) -> Result<Value> {
        let e = obj([
            ("kind", "scope".into()),
            ("participant", who.into()),
            ("owns", scopes.to_vec().into()),
        ]);
        self.append(
            id,
            &e,
            &format!(
                "Scope: {}",
                if scopes.is_empty() {
                    "cleared".to_owned()
                } else {
                    scopes.join(", ")
                }
            ),
        )?;
        Ok(e)
    }
    pub fn describe(&self, id: &str, who: &str, label: &str) -> Result<Value> {
        let e = obj([
            ("kind", "describe".into()),
            ("participant", who.into()),
            ("label", label.into()),
        ]);
        self.append(id, &e, "Work description updated")?;
        Ok(e)
    }
    pub fn leave(&self, id: &str, who: &str) -> Result<Value> {
        self.append(
            id,
            &obj([("kind", "leave".into()), ("participant", who.into())]),
            "Participant left",
        )?;
        Ok(obj([("status", "left".into())]))
    }
    pub fn close(&self, id: &str, who: &str) -> Result<Value> {
        if self.session.child {
            return Err(Error::new(
                "a worker can only message or acknowledge as its admitted room participant",
            ));
        }
        for _ in 0..MAX_HOPS {
            let target = self.settle(id)?;
            let guard = self.storage()?.rollover_lock(&target, self.stop)?;
            if self.resolve(id)? == target {
                guard.check()?;
                self.append(
                    id,
                    &obj([("kind", "closed".into()), ("participant", who.into())]),
                    "Room closed",
                )?;
                return Ok(obj([("status", "closed".into())]));
            }
        }
        Err(Error::new("room forwarding chain too long"))
    }
    pub fn swap_main(
        &self,
        id: &str,
        who: &str,
        provider: &str,
        model: &str,
        attempt: &str,
        harness: &str,
    ) -> Result<Value> {
        let mut e = obj([
            ("kind", "main-swap".into()),
            ("participant", who.into()),
            ("provider", provider.into()),
            ("model", model.into()),
            ("attempt", attempt.into()),
            ("harness", harness.into()),
        ]);
        let (_, target) = self.append(id, &e, &format!("Main swapped to {model}"))?;
        e.set("room", target);
        Ok(e)
    }
    pub fn send(&self, id: &str, send: Send<'_>) -> Result<Value> {
        if send.text.trim().is_empty() || send.text.len() > 4000 {
            return Err(Error::new("message needs 1..4000 UTF-8 bytes"));
        }
        let mut reply = send.reply_to.map(str::to_owned);
        let target = self.resolve(id)?;
        if send.kind == "answer" && reply.is_none() && send.linked {
            let pending = open_questions(
                &project(&self.snapshot(&target)?.rows)?,
                Some(send.sender),
                None,
            )
            .into_iter()
            .filter(|q| q.text("sender") == send.recipient)
            .collect::<Vec<_>>();
            if pending.len() > 1 {
                return Err(Error::new(format!(
                    "{} has {} open questions to you; choose one with --reply-to: {}",
                    send.recipient,
                    pending.len(),
                    pending
                        .iter()
                        .take(8)
                        .map(|q| q.text("id"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            if let Some(q) = pending.first() {
                reply = Some(q.text("id").to_owned());
                self.notices.borrow_mut().push(format!(
                    "room: linked this answer to open question {}",
                    q.text("id")
                ));
            }
        }
        if let Some(mid) = &reply {
            let state = project(&self.snapshot(&target)?.rows)?;
            if !state["predecessor"].is_null()
                && !state.list("messages").iter().any(|m| m.text("id") == mid)
            {
                let found = self.lineage_messages(&state, &BTreeSet::from([mid.clone()]))?;
                if let Some(original) = found.get(mid)
                    && original
                        .list("targets")
                        .iter()
                        .any(|p| p.as_str() == Some(send.sender))
                    && send.recipient == original.text("sender")
                {
                    self.notices.borrow_mut().push(format!("room: {mid} stayed in a sealed predecessor when it rolled over; sending without --reply-to"));
                    reply = None;
                }
            }
        }
        let mut e = obj([
            ("kind", "message".into()),
            (
                "id",
                send.message_id
                    .map(str::to_owned)
                    .unwrap_or(format!("msg-{}", unique()?))
                    .into(),
            ),
            ("sender", send.sender.into()),
            ("recipient", send.recipient.into()),
            ("message_kind", send.kind.into()),
        ]);
        if let Some(v) = reply {
            e.set("reply_to", v);
        }
        if send.quiet {
            e.set("quiet", true);
        }
        match self.append(id, &e, send.text) {
            Ok(_) => (),
            Err(e2) if e2.message.contains("room message limit reached") && !self.session.child => {
                self.rollover(&self.resolve(id)?, None)?;
                self.append(id, &e, send.text)?;
            }
            Err(e) => return Err(e),
        }
        Ok(e)
    }
    pub fn acknowledge(&self, id: &str, who: &str, mids: &[String]) -> Result<()> {
        if mids.is_empty() || mids.len() > 200 {
            return Err(Error::new("ack requires bounded message ids"));
        }
        for mid in mids {
            identifier(mid, "message")?;
        }
        let id = self.resolve(id)?;
        let state = project(&self.snapshot(&id)?.rows)?;
        let known: BTreeSet<_> = state
            .list("messages")
            .iter()
            .map(|m| m.text("id").to_owned())
            .collect();
        let wanted = mids
            .iter()
            .filter(|mid| !known.contains(*mid))
            .cloned()
            .collect();
        let prior = self.lineage_messages(&state, &wanted)?;
        let selected: Vec<String> = mids
            .iter()
            .filter(|mid| {
                prior.get(*mid).is_none_or(|m| {
                    !m.list("received_by")
                        .iter()
                        .any(|p| p.as_str() == Some(who))
                })
            })
            .cloned()
            .collect();
        if selected.is_empty() {
            return Ok(());
        }
        self.append(
            &id,
            &obj([
                ("kind", "received".into()),
                ("participant", who.into()),
                ("messages", selected.into()),
            ]),
            "Messages consumed",
        )?;
        Ok(())
    }
    pub fn claim_review(&self, id: &str, event: &Value) -> Result<bool> {
        match self.append(id, event, "Automatic review claimed") {
            Ok(_) => Ok(true),
            Err(e) if e.message.contains("review patch digest already claimed") => Ok(false),
            Err(e) => Err(e),
        }
    }
    pub fn fail_review(&self, id: &str, event: &Value) -> Result<()> {
        self.append(id, event, "Automatic review launch failed")?;
        Ok(())
    }
    pub fn messages(&self, id: &str) -> Result<Vec<Value>> {
        Ok(project(&self.snapshot(&self.resolve(id)?)?.rows)?
            .list("messages")
            .to_vec())
    }
    pub fn status(&self, id: &str, exact: bool) -> Result<Value> {
        let id = if exact {
            id.to_owned()
        } else {
            self.resolve(id)?
        };
        let snapshot = self.snapshot(&id)?;
        self.status_rows(&id, &snapshot.rows)
    }
    pub(crate) fn status_rows(&self, id: &str, rows: &[Value]) -> Result<Value> {
        let mut state = project(rows)?;
        if !state["successor"].is_null() {
            self.later_receipts(&mut state)?;
        }
        let messages = state.list("messages").to_vec();
        let live: BTreeSet<_> = state
            .list("participants")
            .iter()
            .filter(|p| p.flag("joined"))
            .map(|p| p.text("participant").to_owned())
            .collect();
        state.set("message_count", messages.len());
        state.set(
            "pending_count",
            messages
                .iter()
                .map(|m| {
                    m.list("pending_for")
                        .iter()
                        .filter(|p| live.contains(p.as_str().unwrap_or("")))
                        .count()
                })
                .sum::<usize>(),
        );
        let mut statuses = obj([]);
        let mut results = obj([]);
        let parents: BTreeMap<_, _> = state
            .list("participants")
            .iter()
            .filter(|p| p.text("role") != "main")
            .map(|p| {
                (
                    p.text("participant").to_owned(),
                    p.text("parent").to_owned(),
                )
            })
            .collect();
        for m in &messages {
            if m.text("message_kind") == "status" {
                statuses.set(
                    m.text("sender"),
                    obj([
                        ("text", m["text"].clone()),
                        ("ts", m["ts"].clone()),
                        ("seq", m["seq"].clone()),
                    ]),
                );
            }
            if let Some(call) = m.text("id").strip_prefix("result-")
                && m.text("sender") == call
                && m.text("message_kind") == "handoff"
                && parents.get(call).is_some_and(|p| p == m.text("recipient"))
            {
                let code = m.text("text").strip_prefix("Call exit=").and_then(|rest| {
                    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    if digits.is_empty()
                        || digits.len() > 4
                        || rest[digits.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    {
                        None
                    } else {
                        digits.parse::<i64>().ok()
                    }
                });
                results.set(
                    call,
                    obj([
                        ("exit", code.map(Value::from).unwrap_or(Value::Null)),
                        ("ts", m["ts"].clone()),
                        ("seq", m["seq"].clone()),
                        ("read", m.list("pending_for").is_empty().into()),
                    ]),
                );
            }
        }
        for row in self.structured_rows(id, rows)? {
            let e = &row["room_event"];
            if e.text("kind") == "call-result" {
                let old = &results[e.text("call_id")];
                let mut result = e.clone();
                result.set(
                    "ts",
                    old.get("ts").cloned().unwrap_or_else(|| row["ts"].clone()),
                );
                result.set(
                    "seq",
                    old.get("seq")
                        .cloned()
                        .unwrap_or_else(|| row["seq"].clone()),
                );
                result.set("read", old.get("read").cloned().unwrap_or(true.into()));
                results.set(e.text("call_id"), result);
            }
        }
        let mut questions = open_questions(&state, None, None);
        questions.truncate(32);
        for q in &mut questions {
            q.remove("age");
        }
        state.set("statuses", statuses);
        state.set("open_questions", questions);
        state.set("call_results", results);
        state.set(
            "received_count",
            messages
                .iter()
                .map(|m| m.list("received_by").len())
                .sum::<usize>(),
        );
        state.set(
            "answered_count",
            messages.iter().filter(|m| m.flag("answered")).count(),
        );
        let mut pairs: Vec<Value> = Vec::new();
        for m in &messages {
            for target in m.list("targets") {
                let target = target
                    .as_str()
                    .ok_or_else(|| Error::new("invalid target"))?;
                let at = pairs
                    .iter()
                    .position(|p| {
                        p.text("sender") == m.text("sender") && p.text("recipient") == target
                    })
                    .unwrap_or_else(|| {
                        pairs.push(obj([
                            ("sender", m["sender"].clone()),
                            ("recipient", target.into()),
                            ("sent", 0u64.into()),
                            ("pending", 0u64.into()),
                        ]));
                        pairs.len() - 1
                    });
                let p = &mut pairs[at];
                p.set("sent", p.number("sent") + 1);
                if m.list("pending_for")
                    .iter()
                    .any(|v| v.as_str() == Some(target))
                {
                    p.set("pending", p.number("pending") + 1);
                }
            }
        }
        state.set("pairs", pairs);
        let mut pending = Vec::new();
        for m in &messages {
            if !m.list("pending_for").is_empty() {
                let mut p = obj([]);
                for k in [
                    "id",
                    "pending_for",
                    "sender",
                    "recipient",
                    "message_kind",
                    "answered",
                    "ts",
                ] {
                    p.set(k, m[k].clone());
                }
                p.set("text", truncate_text(&m["text"], 300));
                pending.push(p);
            }
        }
        let truncated = pending.len() > 500;
        let skip = pending.len().saturating_sub(500);
        state.set(
            "pending_messages",
            pending.into_iter().skip(skip).collect::<Vec<_>>(),
        );
        state.set("pending_messages_truncated", truncated);
        state.set(
            "messages",
            messages
                .into_iter()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>(),
        );
        Ok(state)
    }
    fn later_receipts(&self, state: &mut Value) -> Result<()> {
        let mut current = state.text("id").to_owned();
        let mut seen = BTreeSet::from([current.clone()]);
        let mut next = state["successor"].as_str().map(str::to_owned);
        let mut messages = state.list("messages").to_vec();
        for _ in 0..MAX_HOPS {
            let Some(n) = next else {
                break;
            };
            if seen.contains(&n) || self.predecessor(&n)?.as_deref() != Some(current.as_str()) {
                break;
            }
            for row in &self.snapshot(&n)?.rows {
                let e = &row["room_event"];
                if e.text("kind") != "received" {
                    continue;
                }
                for mid in strings(&e["messages"])? {
                    if let Some(m) = messages.iter_mut().find(|m| {
                        m.text("id") == mid
                            && m.list("targets")
                                .iter()
                                .any(|p| p.as_str() == Some(e.text("participant")))
                    }) {
                        let mut done = strings(&m["received_by"])?;
                        done.push(e.text("participant").to_owned());
                        done.sort();
                        done.dedup();
                        let pending = m
                            .list("pending_for")
                            .iter()
                            .filter(|p| p.as_str() != Some(e.text("participant")))
                            .cloned()
                            .collect::<Vec<_>>();
                        m.set("received_by", done);
                        m.set("pending_for", pending);
                    }
                }
            }
            seen.insert(n.clone());
            current = n;
            next = self.tail_successor(&current)?;
        }
        state.set("messages", messages);
        Ok(())
    }
    pub fn sealed_swaps(&self, id: &str) -> Result<Vec<Value>> {
        Ok(self.status(id, true)?.list("swaps").to_vec())
    }
    pub fn lineage(&self, state: &Value) -> Result<Vec<LineageEntry>> {
        let mut out = Vec::new();
        for old in self.predecessors(state.text("id"), true)? {
            let before = self.status(&old, true)?;
            let mut calls = Vec::new();
            let mut seqs = obj([]);
            for p in before.list("participants") {
                if p.text("parent").is_empty() {
                    continue;
                }
                seqs.set(p.text("participant"), p["seq"].clone());
                let r = &before["call_results"][p.text("participant")];
                if r.is_null() {
                    continue;
                }
                let mut call = p.clone();
                call.set(
                    "state",
                    if r["exit"].as_i64() == Some(0) {
                        "done"
                    } else {
                        "failed"
                    },
                );
                call.set("ended", r["ts"].clone());
                call.set("exit", r["exit"].clone());
                call.set("read", r["read"].clone());
                call.set("attempt", Value::Null);
                call.set("title", p["label"].clone());
                call.set("_predecessor", old.clone());
                calls.push(call);
            }
            out.push(LineageEntry {
                id: old,
                calls,
                publications: before["publications"].clone(),
                call_seqs: seqs,
            });
        }
        Ok(out)
    }
    pub fn predecessor_calls(
        &self,
        state: &Value,
        chain: Option<&[LineageEntry]>,
    ) -> Result<Vec<Value>> {
        let loaded = if chain.is_none() {
            self.lineage(state)?
        } else {
            Vec::new()
        };
        let chain = chain.unwrap_or(&loaded);
        let here: BTreeSet<_> = state
            .list("participants")
            .iter()
            .map(|p| p.text("participant"))
            .collect();
        let mut calls: BTreeMap<String, Value> = BTreeMap::new();
        for e in chain {
            for c in &e.calls {
                if !here.contains(c.text("participant")) {
                    calls
                        .entry(c.text("participant").to_owned())
                        .or_insert_with(|| c.clone());
                }
            }
        }
        let mut calls = calls.into_values().collect::<Vec<_>>();
        calls.sort_by(|a, b| {
            (b.text("ended"), b.number("seq")).cmp(&(a.text("ended"), a.number("seq")))
        });
        Ok(calls)
    }
    pub(crate) fn prior_publication(&self, state: &Value, target: &str) -> Result<Option<Value>> {
        if let Some(p) = state["publications"].get(target) {
            return Ok(Some(p.clone()));
        }
        for old in self.predecessors(state.text("id"), true)? {
            let before = project(&self.snapshot(&old)?.rows)?;
            if let Some(p) = before["publications"].get(target) {
                return Ok(Some(p.clone()));
            }
        }
        Ok(None)
    }
    pub fn publish(
        &self,
        id: &str,
        retry: bool,
        app: App,
        recipient: Option<&str>,
        allow_wakeup: bool,
    ) -> Result<Value> {
        if self.session.child {
            return Err(Error::new("a worker cannot publish room output"));
        }
        if allow_wakeup && app != App::Claude {
            return Err(Error::new("invalid app delivery options"));
        }
        let state = self.status(id, false)?;
        let mut target = "codex".to_owned();
        let mut consumer = None;
        if app == App::Claude {
            let who = recipient
                .ok_or_else(|| Error::new("Claude app publishing needs --to PARTICIPANT"))?;
            let p = member(&state, who)?;
            if p.text("provider") != "claude"
                || p.text("role") != "main"
                || p.text("consumer").is_empty()
            {
                return Err(Error::new(
                    "Claude app delivery needs an enrolled native main",
                ));
            }
            target = format!("claude.{who}");
            consumer = Some(p.text("consumer").to_owned());
            if !allow_wakeup {
                return Ok(obj([
                    ("room", id.into()),
                    ("target", target.into()),
                    (
                        "receipt",
                        obj([
                            ("status", "wakeup_required".into()),
                            ("submitted", false.into()),
                            ("delivery_unknown", false.into()),
                        ]),
                    ),
                    ("deduplicated", false.into()),
                ]));
            }
        }
        let mut lines = vec![
            format!("OMS room: {}", state.text("title")),
            format!("Room: {id}"),
        ];
        for p in state
            .list("participants")
            .iter()
            .filter(|p| p.flag("joined"))
        {
            lines.push(format!(
                "{} / {} / {}",
                p.text("label"),
                p.text("provider"),
                p.text("role")
            ));
        }
        for m in state
            .list("messages")
            .iter()
            .skip(state.list("messages").len().saturating_sub(4))
        {
            lines.push(format!(
                "{} -> {}: {}",
                m.text("sender"),
                m.text("recipient"),
                clean(m.text("text"), 240)
            ));
        }
        lines.push("Consumption is not approval; call exits are not acceptance.".to_owned());
        let text = lines.join("\n");
        let hash = digest(format!("{}{}", text, consumer.as_deref().unwrap_or("")).as_bytes());
        if let Some(prior) = self.prior_publication(&state, &target)?
            && !retry
            && (prior.text("status") == "pending"
                || prior.flag("delivery_unknown")
                || prior.text("digest") == hash && prior.text("status") == "persisted")
        {
            return Ok(obj([
                ("room", id.into()),
                ("target", target.into()),
                ("receipt", prior),
                ("deduplicated", true.into()),
            ]));
        }
        if !self.services.delivery_available(app) {
            return Err(Error::new("app delivery service is unavailable"));
        }
        let mut e = obj([
            ("kind", "publication".into()),
            ("id", format!("publish-{}", unique()?).into()),
            ("digest", hash.into()),
            ("status", "pending".into()),
            ("delivery_unknown", true.into()),
            ("retry", retry.into()),
            ("target", target.clone().into()),
        ]);
        // Intent validation under the append lock serializes concurrent publishers.
        self.append(id, &e, "App delivery intent")?;
        checkpoint(self.stop)?;
        let receipt = self.services.deliver(DeliveryRequest {
            repository: &self.repo,
            app,
            text: &text,
            consumer: consumer.as_deref(),
            allow_wakeup,
        })?;
        identifier(&receipt.status, "delivery status")?;
        if receipt.status == "pending" {
            return Err(Error::new(
                "delivery producer returned an incomplete receipt; durable intent remains uncertain",
            ));
        }
        let mut fields = receipt.fields;
        if fields.as_object().is_none() {
            return Err(Error::new(
                "delivery producer returned invalid receipt fields; durable intent remains uncertain",
            ));
        }
        fields.set("status", receipt.status.clone());
        fields.set("delivery_unknown", receipt.delivery_unknown);
        e.set("status", receipt.status.clone());
        e.set("delivery_unknown", receipt.delivery_unknown);
        if let Some(turn) = receipt.turn_id {
            identifier(&turn, "app turn")?;
            e.set("turn_id", turn.clone());
            fields.set("turn_id", turn);
        }
        self.append(id, &e, &format!("App delivery: {}", receipt.status))?;
        Ok(obj([
            ("room", id.into()),
            ("target", target.into()),
            ("receipt", fields),
            ("deduplicated", false.into()),
        ]))
    }
    pub fn scope_overlaps(
        &self,
        id: &str,
        state: &Value,
        caller: &str,
        scopes: &[String],
    ) -> Result<Vec<Value>> {
        let status = if state.get("call_results").is_none() {
            self.status_rows(id, &self.snapshot(id)?.rows)?
        } else {
            state.clone()
        };
        let joined: BTreeSet<_> = state
            .list("participants")
            .iter()
            .filter(|p| p.flag("joined"))
            .map(|p| p.text("participant"))
            .collect();
        let mut found = Vec::new();
        for p in state.list("participants") {
            let who = p.text("participant");
            if !p.flag("joined") || p.list("owns").is_empty() || who == caller {
                continue;
            }
            if !p.text("parent").is_empty() {
                let result = &status["call_results"][who];
                if !joined.contains(p.text("parent"))
                    || result.text("kind") == "call-result"
                    || result.flag("read")
                {
                    continue;
                }
            }
            for theirs in strings(&p["owns"])? {
                if scopes.iter().any(|s| overlaps(s, &theirs)) {
                    found.push(obj([
                        ("participant", who.into()),
                        ("role", p["role"].clone()),
                        (
                            "task_id",
                            p.get("task_id").cloned().unwrap_or_else(|| who.into()),
                        ),
                        ("scope", theirs.into()),
                        ("repo", crate::value::os_value(self.repo.as_os_str())),
                        ("room", id.into()),
                    ]));
                }
            }
        }
        Ok(found)
    }
    pub fn discover(&self) -> Result<(Vec<Value>, bool)> {
        let Some(store) = &self.store else {
            return Ok((vec![], false));
        };
        store.check()?;
        let (names, mut incomplete) = store.threads.names(MAX_SCAN_ENTRIES)?;
        let mut found = Vec::new();
        let mut bytes = 0;
        let mut active = 0;
        for name in names {
            checkpoint(self.stop)?;
            let Some(name) = name.to_str() else {
                incomplete = true;
                continue;
            };
            let Some(id) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let before_bytes = bytes;
            let failed_read_charge = rustix::fs::statat(
                &store.threads.fd,
                Store::name(id),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .ok()
            .map(|s| {
                (s.st_size.max(0) as usize)
                    .min(2 * crate::MAX_ROW)
                    .min(MAX_SCAN_BYTES.saturating_sub(bytes) + 1)
            })
            .unwrap_or(0);
            let result = (|| {
                let (header, used) =
                    self.header_bounded(id, MAX_SCAN_BYTES.saturating_sub(bytes))?;
                bytes += used;
                if bytes > MAX_SCAN_BYTES {
                    return Err(Error::new("room discovery byte limit"));
                }
                if header.first().is_none_or(|r| r.text("thread") != id) {
                    return Err(Error::new("invalid discovery header"));
                }
                let Some(created) = header.get(1) else {
                    return Ok(None);
                };
                let e = &created["room_event"];
                if e.is_null() || e.as_object().is_some_and(Vec::is_empty) {
                    return Ok(None);
                }
                if e.text("kind") != "created" {
                    return Err(Error::new("missing room creation event"));
                }
                if let Some(next) = self.tail_successor(id)? {
                    return Ok(Some(obj([
                        ("schema", 1u64.into()),
                        ("kind", "oms-room".into()),
                        ("id", id.into()),
                        ("title", created["text"].clone()),
                        ("task_id", e["task_id"].clone()),
                        ("creator", e["participant"].clone()),
                        ("predecessor", e["predecessor"].clone()),
                        ("successor", next.into()),
                        ("owner", Value::Null),
                        ("closed", false.into()),
                        ("sealed", true.into()),
                        ("participants", array([])),
                        ("messages", array([])),
                        ("publication", Value::Null),
                        ("publications", obj([])),
                        ("delivery", DELIVERY.into()),
                    ])));
                }
                if active >= MAX_ROOMS {
                    return Err(Error::new("room discovery count limit"));
                }
                let stat = rustix::fs::statat(
                    &store.threads.fd,
                    Store::name(id),
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|_| Error::new("room discovery log unavailable"))?;
                if stat.st_size < 0
                    || (stat.st_size as usize).saturating_sub(used)
                        > MAX_SCAN_BYTES.saturating_sub(bytes)
                {
                    return Err(Error::new("room discovery byte limit"));
                }
                let snap = self.snapshot(id)?;
                bytes += snap.bytes.len().saturating_sub(used);
                let state = project(&snap.rows)?;
                if !state["predecessor"].is_null()
                    && !state.flag("carried")
                    && self.tail_successor(state.text("predecessor"))?.as_deref() != Some(id)
                {
                    return Ok(None);
                }
                active += 1;
                Ok(Some(state))
            })();
            match result {
                Ok(Some(state)) => found.push(state),
                Ok(None) => (),
                Err(_) => {
                    incomplete = true;
                    bytes = bytes.max(before_bytes.saturating_add(failed_read_charge));
                    if bytes >= MAX_SCAN_BYTES || active >= MAX_ROOMS {
                        break;
                    }
                }
            }
        }
        found.sort_by(|a, b| a.text("id").cmp(b.text("id")));
        Ok((found, incomplete))
    }
    pub fn list(&self) -> Result<Value> {
        let (states, incomplete) = self.discover()?;
        let mut rooms = Vec::new();
        for s in states {
            let mut r = obj([]);
            for k in ["id", "title", "owner", "closed"] {
                r.set(k, s[k].clone());
            }
            r.set("message_count", s.list("messages").len());
            if s.flag("sealed") {
                r.set("successor", s["successor"].clone());
            }
            rooms.push(r);
        }
        Ok(obj([
            ("rooms", rooms.into()),
            ("scan_limit", MAX_ROOMS.into()),
            ("incomplete", incomplete.into()),
        ]))
    }
    pub fn selected(
        &self,
        consumer: Option<&str>,
        preferred: Option<&str>,
        who: Option<&str>,
    ) -> Result<(Selection, Option<Value>)> {
        if let Some(id) = preferred {
            let id = self.resolve(identifier(id, "room")?)?;
            let state = project(&self.snapshot(&id)?.rows)?;
            if state.flag("closed") {
                return Ok((Selection::None, Some(state)));
            }
            let p = if let Some(who) = who {
                Some(member(&state, who)?)
            } else {
                state
                    .list("participants")
                    .iter()
                    .find(|p| consumer.is_some_and(|c| p.text("consumer") == c) && p.flag("joined"))
            };
            let found = p
                .map(|p| Selection::Bound {
                    room: id,
                    participant: p.text("participant").to_owned(),
                })
                .unwrap_or(Selection::None);
            return Ok((found, Some(state)));
        }
        let Some(consumer) = consumer else {
            return Ok((Selection::None, None));
        };
        let (states, incomplete) = self.discover()?;
        if incomplete {
            return Ok((Selection::Ambiguous, None));
        }
        let mut enrolled = false;
        let mut candidates = Vec::new();
        for s in states {
            enrolled |= s
                .list("participants")
                .iter()
                .any(|p| p.text("consumer") == consumer || p.text("initial_consumer") == consumer);
            if s.flag("closed") || s.flag("sealed") {
                continue;
            }
            for p in s.list("participants") {
                if p.flag("joined") && p.text("consumer") == consumer {
                    candidates.push((
                        s.text("id").to_owned(),
                        p.text("participant").to_owned(),
                        s.clone(),
                    ));
                }
            }
        }
        if candidates.len() == 1 {
            let (room, participant, state) = candidates.remove(0);
            Ok((Selection::Bound { room, participant }, Some(state)))
        } else {
            Ok((
                if enrolled {
                    Selection::Ambiguous
                } else {
                    Selection::None
                },
                None,
            ))
        }
    }
    pub fn remind(&self, id: &str, who: &str) -> Result<Vec<Value>> {
        let id = self.resolve(id)?;
        let state = project(&self.snapshot(&id)?.rows)?;
        let mut seen: BTreeSet<String> = state
            .list("messages")
            .iter()
            .map(|m| m.text("id").to_owned())
            .collect();
        let mut sent = Vec::new();
        let mut attempts = 0;
        if seen.len() >= MAX_MESSAGES {
            return Ok(sent);
        }
        for mut q in open_questions(&state, None, Some(who)) {
            let age = q.number("age");
            let tier = if age >= SECOND_REMINDER_AFTER {
                2
            } else if age >= ASKER_AFTER {
                1
            } else {
                0
            };
            let mid = reminder_id(q.text("id"), tier);
            if tier == 0 || q.text("recipient") == "all" || seen.contains(&mid) {
                continue;
            }
            if attempts >= MAX_REMINDERS {
                break;
            }
            attempts += 1;
            let text = format!(
                "Reminder: open question {} ({} min)",
                q.text("id"),
                age / 60
            );
            if self
                .send(
                    &id,
                    Send {
                        sender: who,
                        recipient: q.text("recipient"),
                        text: &text,
                        kind: "note",
                        reply_to: None,
                        message_id: Some(&mid),
                        linked: false,
                        quiet: false,
                    },
                )
                .is_ok()
            {
                seen.insert(mid.clone());
                q.set("reminder", mid);
                sent.push(q);
            }
        }
        Ok(sent)
    }
    pub fn read_call_mail(
        &self,
        id: &str,
        who: &str,
        consumed: Option<&[String]>,
    ) -> Result<Value> {
        if self.session.child
            || self.session.participant.as_deref() != Some(who)
            || self.session.admitted_participant.as_deref() != Some(who)
        {
            return Err(Error::new(
                "read-call mail consumption requires the admitted parent dispatcher",
            ));
        }
        let id = self.resolve(id)?;
        if !self.bound(&id)? {
            return Err(Error::new(
                "read-call mail does not belong to the inherited room",
            ));
        }
        let state = project(&self.snapshot(&id)?.rows)?;
        let p = member(&state, who)?;
        if p.text("role") == "main" || p.text("parent").is_empty() {
            return Err(Error::new(
                "read-call mail requires an admitted child participant",
            ));
        }
        let Some(consumed) = consumed else {
            return Ok(self.updates(&id, who, "", 8192, 50)?["turns"].clone());
        };
        if consumed.len() > MAX_MESSAGES {
            return Err(Error::new(
                "read-call consumption requires bounded captured message ids",
            ));
        }
        for mid in consumed {
            identifier(mid, "message")?;
        }
        let captured: BTreeSet<_> = consumed.iter().map(String::as_str).collect();
        let selected = unread_messages(&state, who)?
            .iter()
            .filter(|m| captured.contains(m.text("id")))
            .map(|m| m.text("id").to_owned())
            .collect::<Vec<_>>();
        if !selected.is_empty() {
            self.acknowledge(&id, who, &selected)?;
        }
        Ok(selected.len().into())
    }
}
fn truncate_text(v: &Value, limit: usize) -> Value {
    if let Some(s) = v.as_str() {
        s.chars().take(limit).collect::<String>().into()
    } else {
        match v {
            Value::String(b) => {
                let mut end = 0;
                for _ in 0..limit {
                    if end >= b.len() {
                        break;
                    }
                    end += match b[end] {
                        0..=127 => 1,
                        194..=223 => 2,
                        224..=239 => 3,
                        _ => 4,
                    };
                }
                Value::String(b[..end.min(b.len())].to_vec())
            }
            _ => "".into(),
        }
    }
}

fn check_turn_text(text: &str) -> Result<()> {
    if !crate::outbound_clean(text.as_bytes())? {
        return Err(Error::new(
            "turn looks sensitive; it would be replayed into other agents' prompts",
        ));
    }
    Ok(())
}

fn turn_text(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    let budget = std::env::var("OMS_THREAD_TURN_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map(|n| n.clamp(1, 32768) as usize)
        .unwrap_or(4000);
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut start = text.len() - budget;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[earlier output truncated: see artifact]\n{}",
        &text[start..]
    )
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    #[test]
    fn append_batch_stages_validation_and_preserves_prior_bytes() {
        let path = std::env::temp_dir().join(format!("oms-room-batch-{}", unique().unwrap()));
        std::fs::create_dir(&path).unwrap();
        let result = std::panic::catch_unwind(|| {
            struct Fixture;
            impl Services for Fixture {
                fn canonical_provider(&self, value: &str) -> Result<String> {
                    Ok(value.to_owned())
                }
                fn state_repository(&self, path: &Path) -> Result<PathBuf> {
                    Ok(path.to_owned())
                }
            }
            let stop = AtomicBool::new(false);
            let room = Room::open(&path, Session::default(), &Fixture, &stop, true, false).unwrap();
            room.create(Some("batch"), "Batch", None, Some("owner"), None)
                .unwrap();
            let valid = obj([("kind", "closed".into()), ("participant", "owner".into())]);
            let invalid = obj([("kind", "unknown".into())]);
            let before = std::fs::read(path.join(".oms/threads/batch.jsonl")).unwrap();
            assert!(
                room.append_batch_at(
                    "batch",
                    &[
                        (&valid, "Close", Carry::No),
                        (&invalid, "Invalid", Carry::No)
                    ]
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(path.join(".oms/threads/batch.jsonl")).unwrap(),
                before
            );
            let refused = ["pass", "word", "=", "abcdefghijklmnop"].concat();
            assert!(
                room.append_batch_at("batch", &[(&valid, &refused, Carry::No)])
                    .is_err()
            );
            assert_eq!(
                std::fs::read(path.join(".oms/threads/batch.jsonl")).unwrap(),
                before
            );
            assert!(room.append_batch_at("batch", &[]).unwrap().is_empty());
            assert!(
                room.append_batch_at("batch", &[(&valid, "", Carry::No)])
                    .is_err()
            );
            assert_eq!(
                room.append_batch_at("batch", &[(&valid, "Close", Carry::No)])
                    .unwrap(),
                vec![true]
            );
            let after = std::fs::read(path.join(".oms/threads/batch.jsonl")).unwrap();
            assert!(after.starts_with(&before));
            assert_eq!(
                room.append_batch_at("batch", &[(&valid, "Close", Carry::No)])
                    .unwrap(),
                vec![false]
            );
            assert_eq!(
                std::fs::read(path.join(".oms/threads/batch.jsonl")).unwrap(),
                after
            );
        });
        std::fs::remove_dir_all(path).unwrap();
        result.unwrap();
    }
}
