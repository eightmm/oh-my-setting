//! Private user route table mapping an external team to an existing OMS Room target.
//!
//! One `serve` can then feed many repositories from a single tracker connection. The table is
//! `$XDG_CONFIG_HOME/oh-my-setting/intake-routes.json` (or `~/.config/...`, or the absolute
//! `OMS_INTAKE_ROUTES_FILE`); it must be a private regular file of the current user in a
//! directory no one else can write, and edits are serialized by an adjacent lock file.

use crate::model::validate_source_name;
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

pub const ENV_ROUTES_FILE: &str = "OMS_INTAKE_ROUTES_FILE";
pub const DEFAULT_ROUTES_REL: &str = "oh-my-setting/intake-routes.json";
/// Team value of the fallback route used when no other route matches.
pub const ANY_TEAM: &str = "*";
const MAX_ROUTES: usize = 64;
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    pub source: String,
    pub team: String,
    pub repo: PathBuf,
    pub room: String,
    pub participant: String,
    pub to: String,
}

impl Route {
    /// The single-target `serve --repo` binding: one route that matches every team.
    pub fn any(source: &str, repo: PathBuf, room: String, participant: String, to: String) -> Self {
        Self {
            source: source.to_owned(),
            team: ANY_TEAM.to_owned(),
            repo,
            room,
            participant,
            to,
        }
    }

    fn validate(&self) -> Result<(), String> {
        validate_source_name(&self.source)?;
        validate_team(&self.team)?;
        if !self.repo.is_absolute() {
            return Err(format!(
                "route {}: repo must be an absolute path",
                self.team
            ));
        }
        oms_command_room::identifier(&self.room, "room").map_err(|e| e.message)?;
        oms_command_room::identifier(&self.participant, "participant").map_err(|e| e.message)?;
        if self.to != "all" {
            oms_command_room::identifier(&self.to, "recipient").map_err(|e| e.message)?;
        }
        if self.participant == self.to {
            return Err("--participant and --to must name distinct Room participants".into());
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "source": self.source,
            "team": self.team,
            "repo": self.repo.to_string_lossy(),
            "room": self.room,
            "participant": self.participant,
            "to": self.to,
        })
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        let field = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("route table entry is missing string field {name}"))
        };
        let route = Self {
            source: field("source")?,
            team: field("team")?,
            repo: PathBuf::from(field("repo")?),
            room: field("room")?,
            participant: field("participant")?,
            to: field("to")?,
        };
        route.validate()?;
        Ok(route)
    }
}

/// A team key (`JAE`), a team id, or `*` for the fallback route.
pub fn validate_team(team: &str) -> Result<(), String> {
    if team == ANY_TEAM
        || (!team.is_empty()
            && team.len() <= 64
            && !team.starts_with('-')
            && team
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    {
        Ok(())
    } else {
        Err(format!(
            "invalid team '{team}': use a team key or id ([A-Za-z0-9_-], at most 64) or '*'"
        ))
    }
}

/// The first route whose team equals one of `keys`, else the source's `*` route.
pub fn resolve<'a>(routes: &'a [Route], source: &str, keys: &[String]) -> Option<&'a Route> {
    let mine = || routes.iter().filter(move |r| r.source == source);
    keys.iter()
        .find_map(|key| mine().find(|r| r.team == *key))
        .or_else(|| mine().find(|r| r.team == ANY_TEAM))
}

pub fn routes_path() -> Result<PathBuf, String> {
    if let Some(raw) = std::env::var_os(ENV_ROUTES_FILE) {
        let path = PathBuf::from(raw);
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(format!("{ENV_ROUTES_FILE} must be an absolute file path"));
        }
        return Ok(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| {
            format!("cannot locate the route table: set XDG_CONFIG_HOME, HOME or {ENV_ROUTES_FILE}")
        })?;
    Ok(base.join(DEFAULT_ROUTES_REL))
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn check_dir(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("route table directory unavailable: {e}"))?;
    if !meta.is_dir() || meta.uid() != uid() || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "route table directory {} must be a directory owned by you and not group/world-writable",
            dir.display()
        ));
    }
    Ok(())
}

/// Read the table; a missing file is an empty table.
pub fn load(path: &Path) -> Result<Vec<Route>, String> {
    let Some(dir) = path.parent() else {
        return Err("route table path has no parent directory".into());
    };
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => check_dir(dir)?,
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("route table {} unreadable: {e}", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("route table unreadable: {e}"))?;
    if !meta.is_file() || meta.uid() != uid() || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err(format!(
            "route table {} must be a single-link regular file owned by you with mode 0600",
            path.display()
        ));
    }
    if meta.len() > MAX_BYTES {
        return Err("route table is larger than 64 KiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("route table unreadable: {e}"))?;
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> Result<Vec<Route>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "route table is not valid JSON".to_owned())?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("route table version must be 1".into());
    }
    let rows = value
        .get("routes")
        .and_then(Value::as_array)
        .ok_or_else(|| "route table has no routes array".to_owned())?;
    if rows.len() > MAX_ROUTES {
        return Err(format!("route table holds more than {MAX_ROUTES} routes"));
    }
    let mut routes: Vec<Route> = Vec::with_capacity(rows.len());
    for row in rows {
        let route = Route::from_json(row)?;
        if routes
            .iter()
            .any(|r| r.source == route.source && r.team == route.team)
        {
            return Err(format!(
                "route table has two routes for {} team {}",
                route.source, route.team
            ));
        }
        routes.push(route);
    }
    Ok(routes)
}

fn encode(routes: &[Route]) -> Vec<u8> {
    let rows: Vec<Value> = routes.iter().map(Route::to_json).collect();
    let mut out = serde_json::to_vec_pretty(&json!({"version": 1, "routes": rows}))
        .expect("route table JSON encodes");
    out.push(b'\n');
    out
}

/// Run `edit` on the current table under the adjacent lock and publish the result atomically.
/// `edit` returns whether the table changed; an unchanged table is not rewritten.
pub fn update(
    path: &Path,
    edit: impl FnOnce(&mut Vec<Route>) -> Result<bool, String>,
) -> Result<bool, String> {
    let dir = path
        .parent()
        .ok_or_else(|| "route table path has no parent directory".to_owned())?;
    let name = path
        .file_name()
        .ok_or_else(|| "route table path has no file name".to_owned())?
        .to_string_lossy()
        .into_owned();
    if std::fs::symlink_metadata(dir).is_err() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create route table directory: {e}"))?;
    }
    check_dir(dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(dir.join(format!(".{name}.lock")))
        .map_err(|e| format!("route table lock unavailable: {e}"))?;
    rustix::fs::flock(&lock, FlockOperation::LockExclusive)
        .map_err(|e| format!("route table lock unavailable: {e}"))?;

    let mut routes = load(path)?;
    if !edit(&mut routes)? {
        return Ok(false);
    }
    if routes.len() > MAX_ROUTES {
        return Err(format!("route table holds at most {MAX_ROUTES} routes"));
    }
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(OFlags::NOFOLLOW.bits() as i32)
            .open(&tmp)?;
        file.write_all(&encode(&routes))?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write route table: {e}"));
    }
    if let Ok(d) = rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()) {
        let _ = rustix::fs::fsync(&d);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(team: &str, room: &str) -> Route {
        Route {
            source: "linear".into(),
            team: team.into(),
            repo: PathBuf::from("/srv/repo"),
            room: room.into(),
            participant: "bridge".into(),
            to: "main".into(),
        }
    }

    #[test]
    fn resolve_prefers_an_exact_team_then_the_fallback() {
        let routes = vec![route(ANY_TEAM, "fallback"), route("JAE", "jae")];
        let keys = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            resolve(&routes, "linear", &keys(&["JAE", "id-1"]))
                .unwrap()
                .room,
            "jae"
        );
        assert_eq!(
            resolve(&routes, "linear", &keys(&["OPS"])).unwrap().room,
            "fallback"
        );
        assert!(resolve(&routes[1..], "linear", &keys(&["OPS"])).is_none());
        assert!(resolve(&routes, "other", &keys(&["JAE"])).is_none());
    }

    #[test]
    fn parse_refuses_bad_versions_fields_and_duplicate_teams() {
        let one = encode(&[route("JAE", "r")]);
        assert_eq!(parse(&one).unwrap(), vec![route("JAE", "r")]);
        assert!(parse(br#"{"version":2,"routes":[]}"#).is_err());
        assert!(parse(&encode(&[route("JAE", "a"), route("JAE", "b")])).is_err());
        let mut bad = route("bad team", "r");
        assert!(parse(&encode(std::slice::from_ref(&bad))).is_err());
        bad.team = "JAE".into();
        bad.repo = PathBuf::from("relative");
        assert!(parse(&encode(&[bad])).is_err());
        assert!(validate_team("-x").is_err());
        assert!(validate_team("3ab2f60d-b9bc-44c3-ae45-33d562c43ce9").is_ok());
    }
}
