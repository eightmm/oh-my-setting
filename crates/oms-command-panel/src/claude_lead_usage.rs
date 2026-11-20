//! Weekly quota the Claude lead pane's own statusline hook cached.
//!
//! Proof chain: the pane process's kernel start time must equal `procStart` in
//! Claude Code's `sessions/<pid>.json` (an undocumented internal record), whose
//! `sessionId` keys the HUD `ctx-` file the statusline hook writes. Roots come
//! from the lead's initial environment, never the panel's, so a decoy under the
//! panel's own roots is never read.

use oms_panel_core::native::reference::process::Owner;

pub(crate) const OBSERVATION_SWITCH: &str = "OMS_PANEL_CLAUDE_OBSERVATION";
pub(crate) const AUTO_OBSERVATION_SWITCH: &str = "OMS_PANEL_CLAUDE_AUTO_OBSERVATION";
pub(crate) const OBSERVATION_OVERRIDES: &[&str] = &[
    concat!("ANTHROPIC_API_", "KEY"),
    concat!("ANTHROPIC_AUTH_", "TOKEN"),
    concat!("CLAUDE_CODE_OAUTH_", "TOKEN"),
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
];

#[cfg(not(target_os = "linux"))]
pub(crate) fn claude_observation_identity(_: &std::path::Path, _: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{
    AutoObservation, LocalObservation, claude_observation_identity, inspect_observation,
};

#[derive(Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AutoLaunch {
    launch: String,
    home: String,
}

impl std::fmt::Debug for AutoLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AutoLaunch")
    }
}

impl AutoLaunch {
    pub(crate) fn new(path: &std::path::Path, uid: u32) -> Option<Self> {
        use std::io::Read;
        let home = claude_observation_identity(path, uid)?;
        let mut nonce = [0; 32];
        std::fs::File::open("/dev/urandom")
            .ok()?
            .read_exact(&mut nonce)
            .ok()?;
        Some(Self {
            launch: oms_panel_core::digest(&nonce),
            home,
        })
    }

    pub(crate) fn valid(&self) -> bool {
        [&self.launch, &self.home].into_iter().all(|key| {
            key.len() == 64
                && key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    }

    pub(crate) fn matches_home(&self, path: &std::path::Path, uid: u32) -> bool {
        claude_observation_identity(path, uid).as_ref() == Some(&self.home)
    }

    pub(crate) fn marker(&self) -> String {
        format!("auto-{}", self.launch)
    }

    pub(crate) fn switch_value(&self) -> String {
        format!("{}:{}", self.launch, self.home)
    }
}

pub(crate) struct InspectedObservation {
    pub(crate) used_percentage: f64,
    pub(crate) callback_age_seconds: f64,
    pub(crate) resets_at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LeadReading {
    /// `None` when the reading is stale: a stale number is never shown.
    pub(crate) weekly_used: Option<f64>,
    pub(crate) resets_at: Option<f64>,
    pub(crate) observed_at: f64,
    pub(crate) stale: bool,
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn claude_lead_weekly(_pid: u32, _now: f64) -> Option<LeadReading> {
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn bound_claude_lead_weekly(
    _owner: &Owner,
    _config_digest: &str,
    _now: f64,
) -> Option<LeadReading> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{bound_claude_lead_weekly, claude_lead_weekly};

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use super::{AutoLaunch, InspectedObservation, LeadReading, Owner};
    use oms_panel_core::{Value, metrics, terminals};
    use rustix::fs::{self, FileType, Mode, OFlags, Stat};
    use rustix::process::{getegid, geteuid};
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};

    pub(crate) const DIR: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    pub(crate) const FILE: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOCTTY);
    const ENVIRON_LIMIT: u64 = 256 * 1024;
    const SESSION_LIMIT: usize = 16 * 1024;
    const CTX_LIMIT: usize = 8192;
    const CANDIDATE: &str = ".oms-claude-weekly-candidate.json";
    const RECEIPT: &str = ".oms-claude-weekly-receipt.json";
    const AUTO: &str = ".oms-claude-auto-weekly.json";
    const OBSERVATION_LIMIT: usize = 1024;
    const ENV_KEYS: [&str; 4] = ["CLAUDE_CONFIG_DIR", "HOME", "OMS_HUD_CACHE_DIR", "TMPDIR"];

    pub(crate) type LeadEnv = [Option<OsString>; 4];

    #[derive(Debug, PartialEq)]
    pub(super) struct Roots {
        pub(super) config: PathBuf,
        pub(super) hud: PathBuf,
    }

    pub(crate) fn claude_lead_weekly(pid: u32, now: f64) -> Option<LeadReading> {
        let owner = terminals::process(pid).ok()?;
        if owner.uid != geteuid().as_raw() {
            return None;
        }
        let reading = read(pid, start_time(&owner.birth)?, &roots(&environ(pid)?)?, now)?;
        // A pid that exited or was reused while the files were read proves nothing.
        (terminals::process(pid).ok()? == owner).then_some(reading)
    }

    pub(crate) fn bound_claude_lead_weekly(
        expected: &Owner,
        config_digest: &str,
        now: f64,
    ) -> Option<LeadReading> {
        let pid = expected.pid;
        let owner = terminals::process(pid).ok()?;
        if &owner != expected || owner.uid != geteuid().as_raw() {
            return None;
        }
        let selected = roots(&environ(pid)?)?;
        let reading = read_for_config(
            pid,
            start_time(&owner.birth)?,
            &selected,
            now,
            Some(config_digest),
        )?;
        (roots(&environ(pid)?)? == selected && terminals::process(pid).ok()? == owner)
            .then_some(reading)
    }

    /// `Owner::birth` is `{boot_id}:{starttime}` with /proc stat field 22.
    pub(crate) fn start_time(birth: &str) -> Option<&str> {
        birth.rsplit_once(':').map(|(_, start)| start)
    }

    pub(crate) fn environ(pid: u32) -> Option<LeadEnv> {
        parse_environ(&environ_bytes(pid)?)
    }

    fn environ_bytes(pid: u32) -> Option<Vec<u8>> {
        let file = File::open(format!("/proc/{pid}/environ")).ok()?;
        let mut raw = Vec::new();
        file.take(ENVIRON_LIMIT + 1).read_to_end(&mut raw).ok()?;
        if raw.len() as u64 > ENVIRON_LIMIT {
            return None;
        }
        Some(raw)
    }

    pub(crate) fn claude_observation_identity(path: &Path, uid: u32) -> Option<String> {
        mounted_identity(&open_dir(path)?, uid)
    }

    fn mounted_identity(dir: &OwnedFd, uid: u32) -> Option<String> {
        use fs::{AtFlags, StatxFlags};
        let mask = StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID;
        let stat = fs::statx(dir, "", AtFlags::EMPTY_PATH, mask).ok()?;
        if stat.stx_mask & mask.bits() != mask.bits()
            || stat.stx_uid != uid
            || FileType::from_raw_mode(stat.stx_mode.into()) != FileType::Directory
            || stat.stx_mode & 0o022 != 0
            || stat.stx_ino == 0
            || stat.stx_btime.tv_sec <= 0
            || stat.stx_mnt_id == 0
        {
            return None;
        }
        Some(oms_panel_core::digest(
            format!(
                "claude-home\0{}\0{}\0{}\0{}\0{}\0{}",
                stat.stx_dev_major,
                stat.stx_dev_minor,
                stat.stx_ino,
                stat.stx_btime.tv_sec,
                stat.stx_btime.tv_nsec,
                stat.stx_mnt_id
            )
            .as_bytes(),
        ))
    }

    fn same_process_root(pid: u32) -> bool {
        ["root", "ns/mnt"].into_iter().all(|name| {
            let identity = |who: &str| {
                let m = std::fs::metadata(format!("/proc/{who}/{name}")).ok()?;
                Some((m.dev(), m.ino()))
            };
            identity("self").is_some_and(|local| identity(&pid.to_string()) == Some(local))
        })
    }

    fn scoped_observation_session(
        owner: &Owner,
        home: &Path,
        dir: &(OwnedFd, Stat),
        launch: Option<&AutoLaunch>,
        identity: &str,
    ) -> Option<String> {
        let steady = || {
            terminals::process(owner.pid).ok().as_ref() == Some(owner)
                && same_process_root(owner.pid)
        };
        if !steady() {
            return None;
        }
        let env = environ_bytes(owner.pid)?;
        let auto_value = launch.map(AutoLaunch::switch_value);
        let (switch, expected) = match auto_value.as_deref() {
            Some(value) => (super::AUTO_OBSERVATION_SWITCH, value),
            None => (super::OBSERVATION_SWITCH, identity),
        };
        let prefix = format!("{switch}=");
        let mut switches = env
            .split(|b| *b == 0)
            .filter_map(|item| item.strip_prefix(prefix.as_bytes()));
        if switches.next() != Some(expected.as_bytes())
            || switches.next().is_some()
            || (launch.is_some()
                && env
                    .split(|b| *b == 0)
                    .any(|item| item.starts_with(b"OMS_PANEL_CLAUDE_OBSERVATION=")))
            || env.split(|b| *b == 0).any(|entry| {
                entry.iter().position(|b| *b == b'=').is_some_and(|pos| {
                    pos + 1 < entry.len()
                        && super::OBSERVATION_OVERRIDES
                            .iter()
                            .any(|key| key.as_bytes() == &entry[..pos])
                })
            })
            || parse_environ(&env)?[0].as_deref() != Some(home.as_os_str())
        {
            return None;
        }
        let sessions = private_dir(fs::openat(&dir.0, "sessions", DIR, Mode::empty()).ok()?)?;
        let name = format!("{}.json", owner.pid);
        let bytes = read_private(&sessions, &name, SESSION_LIMIT, true)?;
        let record: Value = oms_panel_core::decode(&bytes, SESSION_LIMIT).ok()?;
        let session = native_session(&record, owner.pid, start_time(&owner.birth)?)?;
        let current = private_dir(fs::openat(&dir.0, "sessions", DIR, Mode::empty()).ok()?)?;
        if (sessions.1.st_dev, sessions.1.st_ino) != (current.1.st_dev, current.1.st_ino)
            || read_private(&current, &name, SESSION_LIMIT, true)? != bytes
        {
            return None;
        }
        steady().then(|| oms_panel_core::digest(format!("claude-session\0{session}").as_bytes()))
    }

    fn observation_time() -> Option<f64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs_f64())
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct Observation {
        schema: u8,
        home: String,
        process: String,
        session: String,
        weekly_used: f64,
        callback_observed_at: f64,
        resets_at: f64,
    }

    impl Observation {
        fn active_window(&self) -> bool {
            observation_time().is_some_and(|now| self.active_at(now))
        }

        fn active_at(&self, now: f64) -> bool {
            (0.0..=100.0).contains(&self.weekly_used)
                && (0.0..=now).contains(&self.callback_observed_at)
                && self.resets_at > now
                && self.resets_at <= self.callback_observed_at + 7.0 * 86400.0
        }
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AutoRecord {
        launch: String,
        observation: Observation,
    }

    pub(crate) struct AutoObservation {
        record: AutoRecord,
        bytes: Vec<u8>,
    }

    impl AutoObservation {
        pub(crate) fn read(owner: &Owner, home: &Path, launch: &AutoLaunch) -> Option<Self> {
            let dir = Self::directory(owner, home, launch)?;
            let bytes = observation_slot(&dir, AUTO)??;
            let record =
                serde_json::from_value(oms_panel_core::decode(&bytes, OBSERVATION_LIMIT).ok()?)
                    .ok()?;
            let sample = Self { record, bytes };
            sample.proves(owner, home, launch, &dir)?;
            Some(sample)
        }

        fn directory(owner: &Owner, home: &Path, launch: &AutoLaunch) -> Option<(OwnedFd, Stat)> {
            let dir = private_dir(open_dir(home)?)?;
            (mounted_identity(&dir.0, owner.uid).as_ref() == Some(&launch.home)).then_some(dir)
        }

        fn proves(
            &self,
            owner: &Owner,
            home: &Path,
            launch: &AutoLaunch,
            dir: &(OwnedFd, Stat),
        ) -> Option<()> {
            let reading = &self.record.observation;
            let process = oms_panel_core::digest(
                format!("claude-process\0{}\0{}", owner.pid, owner.birth).as_bytes(),
            );
            if self.record.launch != launch.launch
                || reading.schema != 1
                || reading.home != launch.home
                || reading.process != process
                || scoped_observation_session(owner, home, dir, Some(launch), &launch.home)?
                    != reading.session
            {
                return None;
            }
            let now = observation_time()?;
            (reading.active_at(now)
                && now - reading.callback_observed_at <= metrics::READING_FRESH_SECONDS)
                .then_some(())
        }

        pub(crate) fn finish(
            self,
            owner: &Owner,
            home: &Path,
            launch: &AutoLaunch,
        ) -> Option<Value> {
            let dir = Self::directory(owner, home, launch)?;
            self.proves(owner, home, launch, &dir)?;
            let dir = Self::directory(owner, home, launch)?;
            if observation_slot(&dir, AUTO)?? != self.bytes {
                return None;
            }
            let reading = self.record.observation;
            let now = observation_time()?;
            if !reading.active_at(now)
                || now - reading.callback_observed_at > metrics::READING_FRESH_SECONDS
                || terminals::process(owner.pid).ok()?.ne(owner)
                || !same_process_root(owner.pid)
            {
                return None;
            }
            Some(serde_json::json!({
                "weekly_used": reading.weekly_used,
                "callback_observed_at": reading.callback_observed_at,
                "resets_at": reading.resets_at
            }))
        }
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct ObservationReceipt {
        observation: Observation,
        evidence: String,
        acquired_at: Option<f64>,
        ranking_eligible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_scope: Option<String>,
    }

    impl ObservationReceipt {
        fn valid(&self, now: f64) -> bool {
            let reading = &self.observation;
            self.evidence == "local_statusline"
                && !self.ranking_eligible
                && self.acquired_at.is_none()
                && reading.schema == 1
                && (0.0..=now).contains(&reading.callback_observed_at)
                && (0.0..=100.0).contains(&reading.weekly_used)
                && reading.resets_at > reading.callback_observed_at
                && reading.resets_at <= reading.callback_observed_at + 7.0 * 86400.0
                && [&reading.home, &reading.process, &reading.session]
                    .into_iter()
                    .all(|digest| {
                        digest.len() == 64
                            && digest
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
        }
    }

    fn account_scope(alias: &str, home: &Path) -> String {
        let mut scope = format!("claude-weekly-account\0{alias}\0").into_bytes();
        scope.extend_from_slice(home.as_os_str().as_encoded_bytes());
        oms_panel_core::digest(&scope)
    }

    pub(crate) struct ReceiptSnapshot {
        identity: String,
        bytes: Vec<u8>,
        pub(crate) reading: InspectedObservation,
    }

    impl ReceiptSnapshot {
        pub(crate) fn still_named(&self, home: &Path, uid: u32) -> bool {
            let check = || {
                let dir = private_dir(open_dir(home)?)?;
                Some(
                    mounted_identity(&dir.0, uid)? == self.identity
                        && observation_slot(&dir, RECEIPT)?? == self.bytes
                        && claude_observation_identity(home, uid).as_deref()
                            == Some(&self.identity),
                )
            };
            check() == Some(true)
        }
    }

    pub(crate) fn inspect_observation(
        home: &Path,
        alias: &str,
        uid: u32,
        now: f64,
    ) -> Option<ReceiptSnapshot> {
        if !now.is_finite() {
            return None;
        }
        let dir = private_dir(open_dir(home)?)?;
        let identity = mounted_identity(&dir.0, uid)?;
        let (receipt, bytes) = read_observation_receipt(&dir)??;
        let reading = &receipt.observation;
        let age = now - reading.callback_observed_at;
        if !receipt.valid(now)
            || reading.home != identity
            || reading.resets_at <= now
            || age > metrics::READING_FRESH_SECONDS
            || receipt.account_scope.as_deref() != Some(account_scope(alias, home).as_str())
        {
            return None;
        }
        // The caller re-resolves the registry and the named mounted directory before reporting.
        Some(ReceiptSnapshot {
            identity,
            bytes,
            reading: InspectedObservation {
                used_percentage: reading.weekly_used,
                callback_age_seconds: age,
                resets_at: reading.resets_at,
            },
        })
    }

    pub(crate) struct LocalObservation {
        directory: (OwnedFd, Stat),
        bytes: Vec<u8>,
        observation: Observation,
    }

    impl LocalObservation {
        pub(crate) fn read(owner: &Owner, home: &Path, identity: &str) -> Option<Self> {
            let directory = private_dir(open_dir(home)?)?;
            if mounted_identity(&directory.0, owner.uid).as_deref() != Some(identity) {
                return None;
            }
            let bytes = observation_slot(&directory, CANDIDATE)??;
            let observation: Observation =
                serde_json::from_value(oms_panel_core::decode(&bytes, OBSERVATION_LIMIT).ok()?)
                    .ok()?;
            let process = oms_panel_core::digest(
                format!("claude-process\0{}\0{}", owner.pid, owner.birth).as_bytes(),
            );
            if observation.schema != 1
                || observation.home != identity
                || observation.process != process
                || !observation.active_window()
                || already_recorded(&directory, &observation)?
                || scoped_observation_session(owner, home, &directory, None, identity)?
                    != observation.session
            {
                return None;
            }
            Some(Self {
                directory,
                bytes,
                observation,
            })
        }

        pub(crate) fn publish(self, owner: &Owner, home: &Path, alias: &str) -> Option<()> {
            let dir = &self.directory;
            fs::flock(&dir.0, fs::FlockOperation::NonBlockingLockExclusive).ok()?;
            if already_recorded(dir, &self.observation)?
                || observation_slot(dir, CANDIDATE)?? != self.bytes
                || scoped_observation_session(owner, home, dir, None, &self.observation.home)?
                    != self.observation.session
                || claude_observation_identity(home, owner.uid).as_deref()
                    != Some(&self.observation.home)
                || !self.observation.active_window()
            {
                return None;
            }
            let bytes = serde_json::to_vec(&ObservationReceipt {
                observation: self.observation,
                evidence: "local_statusline".into(),
                acquired_at: None,
                ranking_eligible: false,
                account_scope: Some(account_scope(alias, home)),
            })
            .ok()?;
            publish_slot(&dir.0, RECEIPT, &bytes)
        }
    }

    fn already_recorded(dir: &(OwnedFd, Stat), current: &Observation) -> Option<bool> {
        let Some((prior, _)) = read_observation_receipt(dir)? else {
            return Some(false);
        };
        let now = observation_time()?;
        prior
            .valid(now)
            .then_some(prior.observation.home == current.home && prior.observation.resets_at > now)
    }

    fn read_observation_receipt(
        dir: &(OwnedFd, Stat),
    ) -> Option<Option<(ObservationReceipt, Vec<u8>)>> {
        let Some(raw) = observation_slot(dir, RECEIPT)? else {
            return Some(None);
        };
        let value = oms_panel_core::decode(&raw, OBSERVATION_LIMIT).ok()?;
        if value.get("acquired_at") != Some(&Value::Null) {
            return None;
        }
        Some(Some((serde_json::from_value(value).ok()?, raw)))
    }

    fn observation_slot(dir: &(OwnedFd, Stat), name: &str) -> Option<Option<Vec<u8>>> {
        match fs::statat(&dir.0, name, fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if stat.st_mode & 0o777 == 0o600 => {
                read_private_mode(dir, name, OBSERVATION_LIMIT, false, true).map(Some)
            }
            Err(rustix::io::Errno::NOENT) => Some(None),
            _ => None,
        }
    }

    fn publish_slot(dir: &OwnedFd, name: &str, bytes: &[u8]) -> Option<()> {
        let mut nonce = [0; 16];
        File::open("/dev/urandom")
            .ok()?
            .read_exact(&mut nonce)
            .ok()?;
        let suffix = oms_panel_core::digest(&nonce);
        let tmp = format!(".oms-claude-observation-{}", &suffix[..24]);
        let mode = Mode::from_raw_mode(0o600);
        let fd = fs::openat(
            dir,
            tmp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )
        .ok()?;
        let mut file = File::from(fd);
        let written = fs::fchmod(&file, mode).is_ok()
            && file.write_all(bytes).is_ok()
            && file.sync_all().is_ok()
            && fs::renameat(dir, tmp.as_str(), dir, name).is_ok();
        if !written {
            let _ = fs::unlinkat(dir, tmp.as_str(), fs::AtFlags::empty());
        }
        written.then_some(())
    }

    pub(super) fn parse_environ(raw: &[u8]) -> Option<LeadEnv> {
        let mut found = LeadEnv::default();
        for entry in raw.split(|b| *b == 0) {
            let Some(eq) = entry.iter().position(|b| *b == b'=') else {
                continue;
            };
            if let Some(slot) = ENV_KEYS.iter().position(|k| k.as_bytes() == &entry[..eq]) {
                let value = OsString::from_vec(entry[eq + 1..].to_vec());
                if found[slot].replace(value).is_some() {
                    return None;
                }
            }
        }
        Some(found)
    }

    /// Mirrors the writers: Claude Code's config root and the hook's `hud_dir`.
    pub(super) fn roots(env: &LeadEnv) -> Option<Roots> {
        let [_, _, hud, tmp] = env;
        let absolute = |v: &OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
        let config = config_root(env)?;
        let hud = match hud.as_ref().filter(|v| !v.is_empty()) {
            Some(dir) => absolute(dir)?,
            None => match tmp {
                Some(tmp) => absolute(tmp)?,
                None => PathBuf::from("/tmp"),
            }
            .canonicalize()
            .ok()?
            .join("oh-my-setting-hud"),
        };
        Some(Roots { config, hud })
    }

    /// Claude Code's config root as the lead process resolves it.
    pub(crate) fn config_root(env: &LeadEnv) -> Option<PathBuf> {
        let absolute = |v: &OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
        match (&env[0], &env[1]) {
            (Some(dir), _) => absolute(dir),
            (None, Some(home)) => Some(absolute(home)?.join(".claude")),
            (None, None) => None,
        }
    }

    pub(super) fn read(pid: u32, start: &str, roots: &Roots, now: f64) -> Option<LeadReading> {
        read_for_config(pid, start, roots, now, None)
    }

    pub(super) fn read_for_config(
        pid: u32,
        start: &str,
        roots: &Roots,
        now: f64,
        expected_digest: Option<&str>,
    ) -> Option<LeadReading> {
        if expected_digest.is_some_and(|digest| {
            crate::orchestrator::account_binding::path_digest(&roots.config) != digest
        }) {
            return None;
        }
        let config = private_dir(open_dir(&roots.config)?)?;
        let sessions = private_dir(fs::openat(&config.0, "sessions", DIR, Mode::empty()).ok()?)?;
        let record_name = format!("{pid}.json");
        let record_bytes = read_private(&sessions, &record_name, SESSION_LIMIT, true)?;
        let record: Value = serde_json::from_slice(&record_bytes).ok()?;
        let session = native_session(&record, pid, start)?;
        let hud = private_dir(open_dir(&roots.hud)?)?;
        let name = format!(
            "ctx-{}.json",
            &oms_panel_core::digest(session.as_bytes())[..24]
        );
        let ctx: Value =
            serde_json::from_slice(&read_private(&hud, &name, CTX_LIMIT, false)?).ok()?;
        // Claude can switch its current session while keeping the same process.
        if expected_digest.is_some() {
            let current_config = private_dir(open_dir(&roots.config)?)?;
            let current_sessions =
                private_dir(fs::openat(&current_config.0, "sessions", DIR, Mode::empty()).ok()?)?;
            let current_hud = private_dir(open_dir(&roots.hud)?)?;
            let same_dir = |old: &Stat, current: &Stat| {
                (old.st_dev, old.st_ino) == (current.st_dev, current.st_ino)
            };
            if !same_dir(&config.1, &current_config.1)
                || !same_dir(&sessions.1, &current_sessions.1)
                || !same_dir(&hud.1, &current_hud.1)
                || read_private(&current_sessions, &record_name, SESSION_LIMIT, true)?
                    != record_bytes
            {
                return None;
            }
        } else if read_private(&sessions, &record_name, SESSION_LIMIT, true)? != record_bytes {
            return None;
        }
        let observed_at = ctx["ts"]
            .as_f64()
            .filter(|ts| ts.is_finite() && *ts >= 0.0 && *ts <= now)?;
        let week = &ctx["seven_day"];
        let used = week["used_percentage"]
            .as_f64()
            .filter(|used| (0.0..=100.0).contains(used))?;
        if now - observed_at > metrics::READING_FRESH_SECONDS {
            return Some(LeadReading {
                weekly_used: None,
                resets_at: None,
                observed_at,
                stale: true,
            });
        }
        let resets_at = match &week["resets_at"] {
            Value::Null => None,
            reset => Some(reset.as_f64().filter(|r| r.is_finite())?),
        };
        if resets_at.is_some_and(|reset| reset <= now) {
            return None;
        }
        Some(LeadReading {
            weekly_used: Some(used),
            resets_at,
            observed_at,
            stale: false,
        })
    }

    fn native_session<'a>(record: &'a Value, pid: u32, start: &str) -> Option<&'a str> {
        if record["pid"].as_u64() != Some(u64::from(pid))
            || record["procStart"].as_str() != Some(start)
        {
            return None;
        }
        record["sessionId"].as_str().filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    }

    /// Opens an absolute path without following a symlink at any component.
    pub(crate) fn open_dir(path: &Path) -> Option<OwnedFd> {
        let mut dir = fs::open("/", DIR, Mode::empty()).ok()?;
        if !path.is_absolute() {
            return None;
        }
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => dir = fs::openat(&dir, name, DIR, Mode::empty()).ok()?,
                _ => return None,
            }
        }
        Some(dir)
    }

    fn private_dir(fd: OwnedFd) -> Option<(OwnedFd, Stat)> {
        let stat = fs::fstat(&fd).ok()?;
        (stat.st_uid == geteuid().as_raw() && stat.st_mode & 0o022 == 0).then_some((fd, stat))
    }

    /// Group write passes only when `group_tolerated`, the file carries the
    /// caller's group and its parent admits no other user: Claude Code writes its
    /// session record under the user's umask (0664 under umask 002).
    pub(super) fn exposed(file: &Stat, dir: &Stat, group_tolerated: bool) -> bool {
        write_exposed(file, group_tolerated && dir.st_mode & 0o077 == 0)
    }

    /// Group write passes only for the caller's group beneath an owner-only directory.
    fn write_exposed(entry: &Stat, beneath_owner_only: bool) -> bool {
        entry.st_mode & 0o002 != 0
            || (entry.st_mode & 0o020 != 0
                && !(beneath_owner_only && entry.st_gid == getegid().as_raw()))
    }

    pub(crate) fn unchanged(a: &Stat, b: &Stat) -> bool {
        (a.st_dev, a.st_ino, a.st_size) == (b.st_dev, b.st_ino, b.st_size)
            && (a.st_mtime, a.st_mtime_nsec, a.st_ctime, a.st_ctime_nsec)
                == (b.st_mtime, b.st_mtime_nsec, b.st_ctime, b.st_ctime_nsec)
    }

    fn read_private(
        dir: &(OwnedFd, Stat),
        name: &str,
        limit: usize,
        group_tolerated: bool,
    ) -> Option<Vec<u8>> {
        read_private_mode(dir, name, limit, group_tolerated, false)
    }

    fn read_private_mode(
        dir: &(OwnedFd, Stat),
        name: &str,
        limit: usize,
        group_tolerated: bool,
        exact_mode: bool,
    ) -> Option<Vec<u8>> {
        let fd = fs::openat(&dir.0, name, FILE, Mode::empty()).ok()?;
        let before = fs::fstat(&fd).ok()?;
        let size = usize::try_from(before.st_size)
            .ok()
            .filter(|size| *size <= limit)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
            || before.st_nlink != 1
            || before.st_uid != geteuid().as_raw()
            || exposed(&before, &dir.1, group_tolerated)
            || (exact_mode && before.st_mode & 0o777 != 0o600)
        {
            return None;
        }
        let file = File::from(fd);
        let mut buf = Vec::with_capacity(size + 1);
        (&file).take(size as u64 + 1).read_to_end(&mut buf).ok()?;
        let after = fs::fstat(&file).ok()?;
        let named = !exact_mode
            || fs::statat(&dir.0, name, fs::AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|named| {
                unchanged(&after, &named)
                    && named.st_mode == after.st_mode
                    && named.st_uid == after.st_uid
                    && named.st_nlink == 1
            });
        (named && buf.len() == size && unchanged(&before, &after)).then_some(buf)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::LeadReading;
    use super::linux::{
        Roots, exposed, parse_environ, read, read_for_config, roots, start_time, unchanged,
    };
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    const PID: u32 = 4242;
    const START: &str = "987654";
    const SESSION: &str = "0a1b2c3d-4e5f-6789-abcd-ef0123456789";
    const NOW: f64 = 1_900_000_000.0;

    struct Fixture {
        base: PathBuf,
        roots: Roots,
    }

    fn mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        mode(path, 0o600);
    }

    #[test]
    fn auto_observation_pairs_launch_session_home_and_exact_bytes() {
        use super::{AUTO_OBSERVATION_SWITCH, AutoLaunch, AutoObservation};
        use oms_panel_core::{digest, terminals};
        use serde_json::json;
        const RUN: &str = "OMS_CLAUDE_AUTO_READER_FIXTURE";
        let uid = rustix::process::getuid().as_raw();
        if std::env::var_os(RUN).is_none() {
            let fixture = Fixture::new("auto-observation", 70.0);
            let home = &fixture.roots.config;
            let Some(launch) = AutoLaunch::new(home, uid) else {
                return;
            };
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "claude_lead_usage::tests::auto_observation_pairs_launch_session_home_and_exact_bytes"])
                .env_clear()
                .env(RUN, "1")
                .env("CLAUDE_CONFIG_DIR", home)
                .env(AUTO_OBSERVATION_SWITCH, launch.switch_value())
                .output().unwrap();
            assert!(
                output.status.success(),
                "synthetic auto reader failed: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(!home.join(".oms-claude-weekly-receipt.json").exists());
            return;
        }

        let home = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").unwrap());
        let marker = std::env::var(AUTO_OBSERVATION_SWITCH).unwrap();
        let (launch, mount) = marker.split_once(':').unwrap();
        let launch = AutoLaunch {
            launch: launch.into(),
            home: mount.into(),
        };
        let owner = terminals::process(std::process::id()).unwrap();
        let session = home.join("sessions").join(format!("{}.json", owner.pid));
        let session_bytes = json!({"pid":owner.pid,"procStart":start_time(&owner.birth).unwrap(),"sessionId":SESSION});
        write(&session, &session_bytes.to_string());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as f64;
        let path = home.join(".oms-claude-auto-weekly.json");
        let initial = json!({"launch":launch.launch,"observation":{"schema":1,"home":launch.home,
            "process":digest(format!("claude-process\0{}\0{}", owner.pid, owner.birth).as_bytes()),
            "session":digest(format!("claude-session\0{SESSION}").as_bytes()),
            "weekly_used":14.0,"callback_observed_at":now-10.0,"resets_at":now+3600.0}});
        let read = || AutoObservation::read(&owner, &home, &launch);
        assert!(read().is_none());
        write(&path, &initial.to_string());
        let shown = read().unwrap().finish(&owner, &home, &launch).unwrap();
        assert_eq!(
            shown,
            json!({"weekly_used":14.0,"callback_observed_at":now-10.0,"resets_at":now+3600.0})
        );

        let expected = read().unwrap();
        let mut rewritten = initial.clone();
        rewritten["observation"]["weekly_used"] = json!(17.0);
        let alternate = home.join("alternate");
        write(&alternate, &rewritten.to_string());
        fs::rename(&alternate, &path).unwrap();
        assert!(expected.finish(&owner, &home, &launch).is_none());
        for (key, bad) in [
            ("callback_observed_at", json!(now + 60.0)),
            ("callback_observed_at", json!(now - 601.0)),
            ("resets_at", json!(now - 1.0)),
            ("session", json!("b".repeat(64))),
            ("process", json!("c".repeat(64))),
            ("home", json!("d".repeat(64))),
        ] {
            let mut bad_record = initial.clone();
            bad_record["observation"][key] = bad;
            write(&path, &bad_record.to_string());
            assert!(read().is_none(), "invalid field {key}");
        }
        let mut other_launch = initial.clone();
        other_launch["launch"] = json!("e".repeat(64));
        write(&path, &other_launch.to_string());
        assert!(read().is_none());
        write(&path, &initial.to_string());
        mode(&path, 0o644);
        assert!(read().is_none());
        fs::rename(&path, &alternate).unwrap();
        symlink(&alternate, &path).unwrap();
        assert!(read().is_none());
        fs::remove_file(&path).unwrap();
        write(&path, &initial.to_string());
        write(
            &session,
            &json!({"pid":owner.pid, "procStart":"wrong","sessionId":SESSION}).to_string(),
        );
        assert!(read().is_none());
        write(&session, &session_bytes.to_string());
        let replaced = home.with_extension("original");
        fs::rename(&home, &replaced).unwrap();
        fs::create_dir(&home).unwrap();
        mode(&home, 0o700);
        fs::copy(replaced.join(".oms-claude-auto-weekly.json"), &path).unwrap();
        assert!(read().is_none());
        fs::remove_dir_all(&home).unwrap();
        fs::rename(&replaced, &home).unwrap();
    }

    #[test]
    fn inspected_receipt_must_still_name_the_same_bytes() {
        use super::linux::{claude_observation_identity, inspect_observation};
        use serde_json::json;
        let fixture = Fixture::new("receipt-replacement", 14.0);
        let home = &fixture.roots.config;
        let uid = rustix::process::getuid().as_raw();
        let Some(identity) = claude_observation_identity(home, uid) else {
            return;
        };
        let mut scope = b"claude-weekly-account\0backup\0".to_vec();
        scope.extend(home.as_os_str().as_encoded_bytes());
        let mut data = json!({"observation":{"schema":1,"home":identity,"process":"a".repeat(64),"session":"b".repeat(64),
            "weekly_used":14.0,"callback_observed_at":NOW-600.0,"resets_at":NOW+3600.0},
            "evidence":"local_statusline","acquired_at":null,"ranking_eligible":false,
            "account_scope":oms_panel_core::digest(&scope)});
        let path = home.join(".oms-claude-weekly-receipt.json");
        write(&path, &data.to_string());
        let receipt = inspect_observation(home, "backup", uid, NOW).unwrap();
        assert!(receipt.still_named(home, uid));
        let replacement = home.join("replacement");
        data["observation"]["weekly_used"] = json!(42.0);
        write(&replacement, &data.to_string());
        fs::rename(&replacement, &path).unwrap();
        assert!(!receipt.still_named(home, uid));
        fs::rename(&path, &replacement).unwrap();
        symlink(&replacement, &path).unwrap();
        assert!(inspect_observation(home, "backup", uid, NOW).is_none());
    }

    impl Fixture {
        /// Modes are explicit so the host umask cannot decide the outcome.
        fn new(label: &str, weekly: f64) -> Self {
            let base = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("oms-lead-{}-{label}", std::process::id()));
            fs::create_dir(&base).unwrap();
            let roots = Roots {
                config: base.join("config"),
                hud: base.join("hud"),
            };
            for dir in [
                &base,
                &roots.config,
                &roots.config.join("sessions"),
                &roots.hud,
            ] {
                fs::create_dir_all(dir).unwrap();
                mode(dir, 0o700);
            }
            let fixture = Self { base, roots };
            fixture.session(&format!(
                r#"{{"pid":{PID},"procStart":"{START}","sessionId":"{SESSION}"}}"#
            ));
            fixture.ctx(&format!(
                r#"{{"ts":{},"seven_day":{{"used_percentage":{weekly},"resets_at":{}}}}}"#,
                NOW - 60.0,
                NOW + 3600.0
            ));
            fixture
        }

        fn session_path(&self) -> PathBuf {
            self.roots.config.join(format!("sessions/{PID}.json"))
        }

        fn ctx_path(&self) -> PathBuf {
            let hash = oms_panel_core::digest(SESSION.as_bytes());
            self.roots.hud.join(format!("ctx-{}.json", &hash[..24]))
        }

        fn session(&self, body: &str) {
            write(&self.session_path(), body);
        }

        fn ctx(&self, body: &str) {
            write(&self.ctx_path(), body);
        }

        fn read(&self) -> Option<LeadReading> {
            read(PID, START, &self.roots, NOW)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn fresh(weekly: f64) -> Option<LeadReading> {
        Some(LeadReading {
            weekly_used: Some(weekly),
            resets_at: Some(NOW + 3600.0),
            observed_at: NOW - 60.0,
            stale: false,
        })
    }

    #[test]
    fn lead_hud_reading_requires_the_full_proof_chain() {
        let session = |body: &'static str| move |f: &Fixture| f.session(body);
        type Case = (&'static str, Box<dyn Fn(&Fixture)>, Option<LeadReading>);
        let cases: Vec<Case> = vec![
            ("match", Box::new(|_| {}), fresh(42.0)),
            (
                "umask-002-session",
                Box::new(|f| mode(&f.session_path(), 0o664)),
                fresh(42.0),
            ),
            (
                "group-reachable-session",
                Box::new(|f| {
                    mode(&f.session_path(), 0o664);
                    mode(&f.roots.config.join("sessions"), 0o750);
                }),
                None,
            ),
            (
                "other-writable-session",
                Box::new(|f| mode(&f.session_path(), 0o602)),
                None,
            ),
            (
                "group-writable-ctx",
                Box::new(|f| mode(&f.ctx_path(), 0o620)),
                None,
            ),
            (
                "group-writable-root",
                Box::new(|f| mode(&f.roots.config, 0o770)),
                None,
            ),
            (
                "other-writable-hud",
                Box::new(|f| mode(&f.roots.hud, 0o702)),
                None,
            ),
            (
                "pid-mismatch",
                Box::new(session(
                    r#"{"pid":4243,"procStart":"987654","sessionId":"0a1b2c3d-4e5f-6789-abcd-ef0123456789"}"#,
                )),
                None,
            ),
            (
                "altered-birth",
                Box::new(session(
                    r#"{"pid":4242,"procStart":"987655","sessionId":"0a1b2c3d-4e5f-6789-abcd-ef0123456789"}"#,
                )),
                None,
            ),
            (
                "invalid-session-id",
                Box::new(session(
                    r#"{"pid":4242,"procStart":"987654","sessionId":"../0a1b2c3d"}"#,
                )),
                None,
            ),
            (
                "session-symlink",
                Box::new(|f| {
                    let real = f.base.join("real.json");
                    fs::rename(f.session_path(), &real).unwrap();
                    symlink(&real, f.session_path()).unwrap();
                }),
                None,
            ),
            (
                "symlinked-hud-root",
                Box::new(|f| {
                    let real = f.base.join("real-hud");
                    fs::rename(&f.roots.hud, &real).unwrap();
                    symlink(&real, &f.roots.hud).unwrap();
                }),
                None,
            ),
            (
                "hard-linked-session",
                Box::new(|f| fs::hard_link(f.session_path(), f.base.join("twin")).unwrap()),
                None,
            ),
            (
                "hard-linked-ctx",
                Box::new(|f| fs::hard_link(f.ctx_path(), f.base.join("twin")).unwrap()),
                None,
            ),
            (
                "oversize-session",
                Box::new(|f| f.session(&format!("{:<16385}", "{}"))),
                None,
            ),
            (
                "oversize-ctx",
                Box::new(|f| f.ctx(&format!("{:<8193}", "{}"))),
                None,
            ),
            (
                "future-ts",
                Box::new(|f| {
                    f.ctx(&format!(
                        r#"{{"ts":{},"seven_day":{{"used_percentage":5}}}}"#,
                        NOW + 1.0
                    ))
                }),
                None,
            ),
            (
                "stale-ts",
                Box::new(|f| {
                    f.ctx(&format!(
                        r#"{{"ts":{},"seven_day":{{"used_percentage":5}}}}"#,
                        NOW - 601.0
                    ))
                }),
                Some(LeadReading {
                    weekly_used: None,
                    resets_at: None,
                    observed_at: NOW - 601.0,
                    stale: true,
                }),
            ),
            (
                "expired-reset",
                Box::new(|f| {
                    f.ctx(&format!(
                        r#"{{"ts":{},"seven_day":{{"used_percentage":5,"resets_at":{NOW}}}}}"#,
                        NOW - 1.0
                    ))
                }),
                None,
            ),
            (
                "no-weekly",
                Box::new(|f| f.ctx(&format!(r#"{{"ts":{}}}"#, NOW - 1.0))),
                None,
            ),
        ];
        for (label, mutate, expected) in cases {
            let fixture = Fixture::new(label, 42.0);
            mutate(&fixture);
            assert_eq!(fixture.read(), expected, "{label}");
            let digest = crate::orchestrator::account_binding::path_digest(&fixture.roots.config);
            assert_eq!(
                read_for_config(PID, START, &fixture.roots, NOW, Some(&digest)),
                expected,
                "bound {label}"
            );
        }
    }

    #[test]
    fn roots_come_from_the_lead_environment_only() {
        let lead = Fixture::new("lead-env", 17.0);
        let decoy = Fixture::new("ambient-decoy", 88.0);
        let env = |pairs: &[(&str, &Path)]| {
            let mut raw = b"PATH=/usr/bin\0".to_vec();
            for (key, value) in pairs {
                raw.extend(format!("{key}={}\0", value.display()).bytes());
            }
            parse_environ(&raw)
        };
        let found = roots(
            &env(&[
                ("CLAUDE_CONFIG_DIR", &lead.roots.config),
                ("HOME", &decoy.base),
                ("OMS_HUD_CACHE_DIR", &lead.roots.hud),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(found, lead.roots);
        assert_eq!(read(4242, "987654", &found, 1_900_000_000.0), fresh(17.0));
        let pinned = crate::orchestrator::account_binding::path_digest(&lead.roots.config);
        let ambient = crate::orchestrator::account_binding::path_digest(&decoy.roots.config);
        assert_eq!(
            read_for_config(PID, START, &found, NOW, Some(&pinned)),
            fresh(17.0)
        );
        assert!(read_for_config(PID, START, &found, NOW, Some(&ambient)).is_none());

        let home = roots(&env(&[("HOME", &lead.base), ("TMPDIR", &lead.base)]).unwrap()).unwrap();
        assert_eq!(home.config, lead.base.join(".claude"));
        assert_eq!(home.hud, lead.base.join("oh-my-setting-hud"));
        let relative = parse_environ(b"HOME=home\0").unwrap();
        let empty = parse_environ(b"HOME=/h\0CLAUDE_CONFIG_DIR=\0").unwrap();
        assert!(roots(&relative).is_none() && roots(&empty).is_none());
        assert!(roots(&parse_environ(b"TMPDIR=/tmp\0").unwrap()).is_none());
        assert!(parse_environ(b"HOME=/a\0HOME=/b\0").is_none());
    }

    #[test]
    fn growth_or_replacement_between_stats_is_detected() {
        let fixture = Fixture::new("growth", 42.0);
        let path = fixture.ctx_path();
        let stat = || rustix::fs::stat(&path).unwrap();
        let before = stat();
        assert!(unchanged(&before, &stat()));
        fs::write(&path, "{\"ts\":1} ").unwrap();
        assert!(!unchanged(&before, &stat()));
        let dir = rustix::fs::stat(&fixture.roots.hud).unwrap();
        assert!(!exposed(&before, &dir, false));
    }

    #[test]
    fn own_process_birth_matches_proc_starttime_and_environ() {
        let pid = std::process::id();
        let stat = fs::read_to_string("/proc/self/stat").unwrap();
        let fields: Vec<_> = stat[stat.rfind(')').unwrap() + 2..]
            .split_whitespace()
            .collect();
        let owner = oms_panel_core::terminals::process(pid).unwrap();
        assert_eq!(start_time(&owner.birth), Some(fields[19]));
        let raw = fs::read(format!("/proc/{pid}/environ")).unwrap();
        let home = std::env::var_os("HOME");
        // Compared without assert_eq so a failure never prints the home path.
        assert!(parse_environ(&raw).unwrap()[1] == home);
    }
}
