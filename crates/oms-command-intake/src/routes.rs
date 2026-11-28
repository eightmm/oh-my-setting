//! Private user route table mapping an external team to an existing OMS Room target.
//!
//! One `serve` can then feed many repositories from a single tracker connection. The table is
//! `$XDG_CONFIG_HOME/oh-my-setting/intake-routes.json` (or `~/.config/...`, or the absolute
//! `OMS_INTAKE_ROUTES_FILE`); it must be a private regular file of the current user below
//! directories no one else can write, and edits are serialized by the OMS state lock.

use crate::model::validate_source_name;
use oms_safe_fs::{ByteError, ByteReadOptions, MutationError, MutationOptions, MutationPhase};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const ENV_ROUTES_FILE: &str = "OMS_INTAKE_ROUTES_FILE";
pub const DEFAULT_ROUTES_REL: &str = "oh-my-setting/intake-routes.json";
/// Team value of the fallback route used when no other route matches.
pub const ANY_TEAM: &str = "*";
const MAX_ROUTES: usize = 64;
const MAX_BYTES: usize = 64 * 1024;

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
        if !self.repo.is_absolute() || self.repo.to_str().is_none() {
            return Err(format!(
                "route {}: repo must be an absolute UTF-8 path",
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
            "repo": self.repo.to_str(),
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
    path_from(|name| std::env::var_os(name))
}

fn path_from(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<PathBuf, String> {
    if let Some(raw) = env(ENV_ROUTES_FILE) {
        let path = PathBuf::from(raw);
        return normalized(&path)
            .then_some(path)
            .ok_or_else(|| format!("{ENV_ROUTES_FILE} must be a normalized absolute file path"));
    }
    // XDG ignores a relative XDG_CONFIG_HOME; an empty or relative HOME fails
    // closed rather than resolving the table inside the working directory.
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            env("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })
        .ok_or_else(|| {
            format!(
                "cannot locate the route table: set an absolute XDG_CONFIG_HOME, HOME or {ENV_ROUTES_FILE}"
            )
        })?;
    let path = base.join(DEFAULT_ROUTES_REL);
    normalized(&path)
        .then_some(path)
        .ok_or_else(|| "route table directory must be a normalized absolute path".into())
}

fn normalized(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let raw = path.as_os_str().as_bytes();
    path.is_absolute()
        && !raw.ends_with(b"/")
        && !raw.ends_with(b"/.")
        && path.file_name().is_some()
        && !path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        && !raw.windows(3).any(|w| w == b"/./")
        && !raw.windows(2).any(|w| w == b"//")
}

/// Root byte operations at the nearest existing ancestor of the table's directory. Every
/// component from `/` down is opened without following symlinks and must be a directory owned
/// by root or you and not group/world-writable (a root-owned sticky directory such as `/tmp`
/// may sit above it). The path is never canonicalized, so a symlink anywhere in it is refused
/// and the normalized spelling names one physical file, which keeps the lock key unique.
fn anchored(path: &Path) -> Result<(oms_safe_fs::Root, PathBuf), String> {
    if !normalized(path) {
        return Err("route table path must be a normalized absolute file path".into());
    }
    let dir = path
        .parent()
        .ok_or_else(|| "route table path has no parent directory".to_owned())?;
    let mut anchor = dir;
    while std::fs::symlink_metadata(anchor).is_err() {
        anchor = anchor
            .parent()
            .ok_or_else(|| "route table directory has no existing ancestor".to_owned())?;
    }
    let rest = path
        .strip_prefix(anchor)
        .map_err(|_| "route table path is not under its ancestor".to_owned())?
        .to_path_buf();
    let root = oms_safe_fs::Root::open_private_workspace(anchor, |_| false)
        .map_err(|_| UNSAFE.to_owned())?;
    Ok((root, rest))
}

const UNSAFE: &str = "route table or a directory above it is not private to you: give a path without symlinks whose every directory is owned by you or root and not group/world-writable; the table must be a single-link regular file owned by you with mode 0600";

fn storage(error: ByteError) -> String {
    match error {
        ByteError::InputTooLarge | ByteError::OutputTooLarge => {
            "route table is larger than 64 KiB".into()
        }
        ByteError::InvalidPath | ByteError::Unsafe => UNSAFE.into(),
        ByteError::Changed => "route table changed while it was read; retry".into(),
        other => format!("route table unavailable: {other:?}"),
    }
}

fn read(root: &oms_safe_fs::Root, rel: &Path) -> Result<Vec<u8>, String> {
    root.read_no_follow_retrying(
        rel,
        ByteReadOptions {
            missing_ok: true,
            max_bytes: MAX_BYTES,
        },
    )
    .map_err(storage)
}

fn decode(bytes: &[u8]) -> Result<Vec<Route>, String> {
    if bytes.is_empty() {
        Ok(Vec::new())
    } else {
        parse(bytes)
    }
}

/// Read the table; a missing file (or directory) is an empty table.
pub fn load(path: &Path) -> Result<Vec<Route>, String> {
    let (root, rel) = anchored(path)?;
    decode(&read(&root, &rel)?)
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

/// Run `edit` on the current table under the OMS state lock for this file and publish the
/// result atomically. `edit` returns whether the table changed; an unchanged table is not
/// rewritten.
pub fn update(
    path: &Path,
    edit: impl FnOnce(&mut Vec<Route>) -> Result<bool, String>,
) -> Result<bool, String> {
    if !normalized(path) {
        return Err("route table path must be a normalized absolute file path".into());
    }
    let settings =
        oms_lock::Settings::from_env().map_err(|_| "invalid OMS lock environment".to_owned())?;
    let _guard = oms_lock::Lock::for_file(path, settings)
        .and_then(|lock| lock.acquire(None))
        .map_err(|e| format!("route table lock unavailable ({e})"))?;
    let (root, rel) = anchored(path)?;
    let current = read(&root, &rel)?;
    let mut routes = decode(&current)?;
    if !edit(&mut routes)? {
        return Ok(false);
    }
    if routes.len() > MAX_ROUTES {
        return Err(format!("route table holds at most {MAX_ROUTES} routes"));
    }
    for route in &routes {
        route.validate()?;
    }
    let encoded = encode(&routes);
    if encoded.len() > MAX_BYTES {
        return Err("route table would exceed 64 KiB".into());
    }
    // Publish only what a later load accepts, including the one-route-per-team rule.
    parse(&encoded)?;
    root.atomic_mutate(
        &rel,
        MutationOptions {
            read: ByteReadOptions {
                missing_ok: true,
                max_bytes: MAX_BYTES,
            },
            create_parent: true,
            max_output: MAX_BYTES,
            ..Default::default()
        },
        |now| {
            if now == current.as_slice() {
                Ok(encoded)
            } else {
                Err("route table changed during the edit; retry".to_owned())
            }
        },
    )
    .map_err(|e| match e {
        MutationError::Transform(message) => message,
        MutationError::Storage {
            phase: MutationPhase::BeforePublication,
            error,
        } => storage(error),
        MutationError::Storage { .. } => {
            "route table may have been written but its durability is unconfirmed; check it with `oms intake route list`".into()
        }
    })?;
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

    #[test]
    fn table_storage_refuses_unsafe_files_and_ancestors_and_serializes_edits() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "oms-intake-routes-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&base)
            .unwrap();
        let mode = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
        };
        let add = |path: &Path, team: &str| {
            update(path, |routes| {
                routes.push(route(team, "r"));
                Ok(true)
            })
        };

        // A missing directory reads as empty; the first edit creates it privately.
        let good = base.join("cfg/oh-my-setting/intake-routes.json");
        assert_eq!(load(&good), Ok(Vec::new()));
        assert_eq!(add(&good, "JAE"), Ok(true));
        let meta = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((meta(good.parent().unwrap()), meta(&good)), (0o700, 0o600));

        // Concurrent edits under the lock lose nothing.
        std::thread::scope(|scope| {
            for i in 0..8 {
                let good = &good;
                scope.spawn(move || add(good, &format!("T{i}")).unwrap());
            }
        });
        assert_eq!(load(&good).unwrap().len(), 9);

        // Leaf: wider mode, a second link, a symlink.
        mode(&good, 0o640);
        assert_eq!(load(&good), Err(UNSAFE.into()));
        assert!(add(&good, "X").is_err());
        mode(&good, 0o600);
        let linked = base.join("cfg/oh-my-setting/second-link");
        std::fs::hard_link(&good, &linked).unwrap();
        assert_eq!(load(&good), Err(UNSAFE.into()));
        std::fs::remove_file(&linked).unwrap();
        let alias = base.join("cfg/oh-my-setting/alias.json");
        symlink(&good, &alias).unwrap();
        assert_eq!(load(&alias), Err(UNSAFE.into()));
        assert!(add(&alias, "X").is_err());

        // Ancestors: a group-writable directory, a symlink to a shared one, a dangling one.
        mode(&base.join("cfg"), 0o770);
        assert_eq!(load(&good), Err(UNSAFE.into()));
        assert!(add(&good, "X").is_err());
        mode(&base.join("cfg"), 0o700);
        assert_eq!(load(&good).unwrap().len(), 9);
        let private_link = base.join("private-link");
        symlink(base.join("cfg"), &private_link).unwrap();
        let through = private_link.join("oh-my-setting/intake-routes.json");
        assert_eq!(load(&through), Err(UNSAFE.into()));
        assert!(add(&through, "X").is_err());
        let doubled = PathBuf::from(good.to_str().unwrap().replace("/cfg/", "/cfg//"));
        assert!(load(&doubled).is_err() && add(&doubled, "X").is_err());
        let shared = base.join("shared");
        std::fs::create_dir(&shared).unwrap();
        mode(&shared, 0o777);
        symlink(&shared, base.join("via")).unwrap();
        let via = base.join("via/oh-my-setting/intake-routes.json");
        assert_eq!(load(&via), Err(UNSAFE.into()));
        assert!(add(&via, "X").is_err());
        assert!(!shared.join("oh-my-setting").exists());
        symlink(base.join("nowhere"), base.join("dangling")).unwrap();
        // Writes validate every route and the one-route-per-team rule before publishing.
        let before = std::fs::read(&good).unwrap();
        let bad = |edit: fn(&mut Vec<Route>)| {
            update(&good, |routes| {
                edit(routes);
                Ok(true)
            })
        };
        use std::os::unix::ffi::OsStrExt as _;
        assert!(
            bad(|r| r[0].repo = PathBuf::from(std::ffi::OsStr::from_bytes(b"/srv/\xff"))).is_err()
        );
        assert!(bad(|r| r.push(r[0].clone())).is_err());
        assert!(bad(|r| r[0].team = "bad team".into()).is_err());
        assert_eq!(std::fs::read(&good).unwrap(), before);
        assert!(load(&base.join("dangling/oh-my-setting/intake-routes.json")).is_err());

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn route_table_path_needs_an_absolute_root() {
        let at = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect();
            path_from(move |name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.into()))
        };
        let table = |root: &str| Ok(PathBuf::from(root).join(DEFAULT_ROUTES_REL));
        assert_eq!(
            at(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")]),
            table("/x")
        );
        assert_eq!(
            at(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")]),
            table("/h/.config")
        );
        assert_eq!(
            at(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")]),
            table("/h/.config")
        );
        for env in [
            &[("HOME", "")][..],
            &[("HOME", "rel")],
            &[("XDG_CONFIG_HOME", "rel")],
            &[],
        ] {
            assert!(at(env).is_err(), "{env:?}");
        }
        assert!(at(&[("XDG_CONFIG_HOME", "/x/../y")]).is_err());
        assert_eq!(
            at(&[(ENV_ROUTES_FILE, "/t/r.json")]),
            Ok(PathBuf::from("/t/r.json"))
        );
        for bad in [
            "r.json",
            "/t/",
            "/t/.",
            "/t/./r.json",
            "/t/../r.json",
            "//t/r.json",
            "/t//r.json",
        ] {
            assert!(at(&[(ENV_ROUTES_FILE, bad)]).is_err(), "{bad}");
        }
    }
}
