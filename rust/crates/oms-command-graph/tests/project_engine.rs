#![forbid(unsafe_code)]
use oms_command_graph::{
    project::{self, index::Graph},
    run,
};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-project-engine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("tests")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        fs::write(root.join("lib.sh"), "# Parse records.\nparseRecord() {\n  printf '%s' 'alpha record'\n}\nparse_again() {\n  parseRecord\n}\n").unwrap();
        fs::write(
            root.join("tests/test-lib.sh"),
            "source lib.sh\ntest_parse() {\n  parseRecord\n}\n",
        )
        .unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(root.join("tests"), fs::Permissions::from_mode(0o700)).unwrap();
        let old = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for path in ["lib.sh", "tests/test-lib.sh"] {
            fs::set_permissions(root.join(path), fs::Permissions::from_mode(0o600)).unwrap();
            set_mtime(&root.join(path), old);
        }
        Self(root)
    }
    fn state(&self) -> PathBuf {
        self.0.join(".oms/project-graph")
    }
    fn build(&self) -> Value {
        project::build(
            &self.0,
            &self.state(),
            &Default::default(),
            &AtomicBool::new(false),
            None,
        )
        .unwrap()
    }
    fn graph(&self) -> Graph {
        Graph::new(project::load(&self.state()).unwrap()).unwrap()
    }
    fn query(&self, args: &[&str]) -> (i32, Value) {
        let mut argv = vec![OsString::from("project")];
        argv.extend(args.iter().map(OsString::from));
        argv.push("--json".into());
        let (code, out, err) = run(&argv, &self.0, &AtomicBool::new(false));
        if code != 0 {
            return (code, json!(String::from_utf8_lossy(&err)));
        }
        (code, serde_json::from_slice(&out).unwrap())
    }
    fn manifest(&self, value: &Value) {
        fs::write(self.state().join("manifest.json"), value.to_string()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn set_mtime(path: &Path, time: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

#[test]
fn stat_gate_legacy_racy_force_and_prefix_boundaries() {
    let f = Fixture::new();
    fs::write(f.0.join("empty.dat"), []).unwrap();
    fs::write(f.0.join("equal.dat"), vec![b'a'; 8192]).unwrap();
    let mut prefix = vec![b'a'; 8193];
    prefix[8192] = 0;
    fs::write(f.0.join("beyond.dat"), &prefix).unwrap();
    prefix[8191] = 0;
    fs::write(f.0.join("binary.dat"), &prefix).unwrap();
    let manifest = f.build();
    assert!(manifest["files"]["lib.sh"]["mtime_sec"].is_i64());
    assert!(manifest["files"]["lib.sh"]["mtime_nsec"].is_i64());
    assert!(manifest["files"]["lib.sh"]["ino"].is_u64());
    for (path, reason) in [
        ("empty.dat", "unparsed"),
        ("equal.dat", "unparsed"),
        ("beyond.dat", "unparsed"),
        ("binary.dat", "binary"),
    ] {
        assert!(
            manifest["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == path && r["reason"] == reason)
        );
    }
    let source = f.0.join("lib.sh");
    let original = fs::read(&source).unwrap();
    let modified = String::from_utf8(original.clone())
        .unwrap()
        .replace("alpha", "bravo");
    fs::write(&source, &modified).unwrap();
    set_mtime(&source, UNIX_EPOCH + Duration::from_secs(1_000_000));
    let cancel = AtomicBool::new(false);
    assert_eq!(
        project::check(&f.0, &f.state(), &cancel).unwrap()["fresh"],
        true,
        "unchanged stat must reuse digest"
    );
    for field in ["bytes", "ino", "mtime_sec", "mtime_nsec"] {
        let mut changed_stat = manifest.clone();
        changed_stat["files"]["lib.sh"][field] =
            json!(changed_stat["files"]["lib.sh"][field].as_u64().unwrap() + 1);
        f.manifest(&changed_stat);
        assert_eq!(
            project::check(&f.0, &f.state(), &cancel).unwrap()["fresh"],
            false,
            "changed {field} must hash"
        );
    }
    let mut legacy = manifest.clone();
    for key in ["mtime_sec", "mtime_nsec", "ino"] {
        legacy["files"]["lib.sh"]
            .as_object_mut()
            .unwrap()
            .remove(key);
    }
    f.manifest(&legacy);
    assert_eq!(
        project::check(&f.0, &f.state(), &cancel).unwrap()["stale"],
        json!(["lib.sh"])
    );
    let mut racy = manifest.clone();
    racy["build_started_sec"] = json!(1_000_000);
    f.manifest(&racy);
    assert_eq!(
        project::check(&f.0, &f.state(), &cancel).unwrap()["stale"],
        json!(["lib.sh"]),
        "equal-second timestamps must hash"
    );
    racy["build_started_sec"] = json!(999_999);
    f.manifest(&racy);
    assert_eq!(
        project::check(&f.0, &f.state(), &cancel).unwrap()["fresh"],
        false,
        "future timestamps must hash"
    );
    f.manifest(&manifest);
    let rebuilt = project::build(
        &f.0,
        &f.state(),
        &project::BuildOptions {
            force: true,
            ..Default::default()
        },
        &cancel,
        None,
    )
    .unwrap();
    assert_ne!(
        rebuilt["files"]["lib.sh"]["sha256"],
        manifest["files"]["lib.sh"]["sha256"]
    );
    assert_eq!(rebuilt["stats"]["cached"], 0);
    let mut malformed = rebuilt;
    malformed["files"]["lib.sh"]["ino"] = json!("bad");
    f.manifest(&malformed);
    assert_eq!(
        project::check(&f.0, &f.state(), &cancel).unwrap()["fresh"],
        true
    );
    malformed["files"]["lib.sh"]["sha256"] = json!("");
    f.manifest(&malformed);
    assert_eq!(
        project::check(&f.0, &f.state(), &cancel).unwrap()["fresh"],
        false
    );
    fs::write(source, original).unwrap();
}

#[test]
fn sqlite_queries_match_graph_and_fall_back_without_sidecars() {
    let f = Fixture::new();
    f.build();
    let mut payload = project::load(&f.state()).unwrap();
    let db = rusqlite::Connection::open(f.state().join("index.sqlite3")).unwrap();
    for (source, target, relation, confidence) in [
        (
            "symbol:lib.sh::parse_again",
            "symbol:lib.sh::parseRecord",
            "calls",
            "AMBIGUOUS",
        ),
        (
            "symbol:lib.sh::parse_again",
            "symbol:lib.sh::parseRecord",
            "calls",
            "EXTRACTED",
        ),
        (
            "symbol:lib.sh::parseRecord",
            "symbol:lib.sh::parse_again",
            "calls",
            "EXTRACTED",
        ),
        ("symbol:lib.sh::parseRecord", "missing", "uses", "INFERRED"),
    ] {
        let edge = json!({"source":source,"target":target,"relation":relation,"confidence":confidence,"evidence":{"path":"lib.sh","line":6,"extra":"discard"}});
        db.execute(
            "INSERT INTO edges VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![source, target, relation, confidence, edge.to_string()],
        )
        .unwrap();
        payload["edges"].as_array_mut().unwrap().push(edge);
    }
    drop(db);
    fs::write(f.state().join("graph.json"), payload.to_string()).unwrap();
    let graph = f.graph();
    let cancel = AtomicBool::new(false);
    let mut cases = vec![];
    for name in [
        "parseRecord",
        "PARSERECORD",
        "symbol:lib.sh::parseRecord",
        "missing",
        "",
    ] {
        if !name.is_empty() {
            cases.push((
                vec!["symbol", name],
                graph.symbol(&f.0, name, 40, &cancel).unwrap(),
            ));
        }
    }
    for path in ["lib.sh", "tests/test-lib.sh"] {
        cases.push((
            vec!["api", path, "--no-refresh"],
            graph.file_api(path, 500).unwrap(),
        ));
    }
    for direction in ["in", "out", "both"] {
        cases.push((
            vec![
                "neighbors",
                "symbol:lib.sh::parseRecord",
                "--direction",
                direction,
                "--no-refresh",
            ],
            json!(
                graph
                    .neighbors("symbol:lib.sh::parseRecord", "", direction)
                    .unwrap()
            ),
        ));
    }
    for depth in ["0", "1", "2"] {
        for direction in ["in", "out"] {
            cases.push((
                vec![
                    "trace",
                    "symbol:lib.sh::parseRecord",
                    "--direction",
                    direction,
                    "--depth",
                    depth,
                    "--no-refresh",
                ],
                graph
                    .trace(
                        "symbol:lib.sh::parseRecord",
                        direction,
                        depth.parse().unwrap(),
                        &[],
                        128,
                        160,
                    )
                    .unwrap(),
            ));
        }
    }
    let graph_bytes = fs::read(f.state().join("graph.json")).unwrap();
    let image = fs::read(f.state().join("index.sqlite3")).unwrap();
    let before = snapshot(&f.0);
    fs::write(f.state().join("graph.json"), "{").unwrap();
    for (args, expected) in &cases {
        assert_eq!(f.query(args), (0, expected.clone()), "SQLite-only {args:?}");
    }
    assert_eq!(
        snapshot(&f.0),
        before,
        "queries must not add state files or sidecars"
    );
    fs::write(f.state().join("graph.json"), graph_bytes).unwrap();
    for variant in [
        "corrupt", "stale", "schema", "shape", "row", "missing", "symlink",
    ] {
        fs::write(f.state().join("index.sqlite3"), &image).unwrap();
        match variant {
            "corrupt" => fs::write(f.state().join("index.sqlite3"), "not sqlite").unwrap(),
            "stale" | "schema" => {
                let db = rusqlite::Connection::open(f.state().join("index.sqlite3")).unwrap();
                db.execute(
                    "UPDATE metadata SET value='invalid' WHERE key=?1",
                    [if variant == "stale" {
                        "revision"
                    } else {
                        "schema"
                    }],
                )
                .unwrap();
            }
            "shape" | "row" => {
                let db = rusqlite::Connection::open(f.state().join("index.sqlite3")).unwrap();
                db.execute(
                    if variant == "shape" {
                        "DROP TABLE edges"
                    } else {
                        "UPDATE nodes SET data='{' WHERE id='file:lib.sh'"
                    },
                    [],
                )
                .unwrap();
            }
            "missing" | "symlink" => {
                fs::remove_file(f.state().join("index.sqlite3")).unwrap();
                if variant == "symlink" {
                    std::os::unix::fs::symlink("graph.json", f.state().join("index.sqlite3"))
                        .unwrap();
                }
            }
            _ => unreachable!(),
        }
        for (args, expected) in &cases {
            assert_eq!(f.query(args), (0, expected.clone()), "{variant}: {args:?}");
        }
        if variant == "symlink" {
            fs::remove_file(f.state().join("index.sqlite3")).unwrap();
        }
    }
    assert_eq!(f.query(&["api", "../invalid", "--no-refresh"]).0, 2);
    assert_eq!(f.query(&["neighbors", "missing", "--no-refresh"]).0, 2);
    assert_eq!(
        f.query(&["trace", "missing", "--depth", "0", "--no-refresh"])
            .0,
        2
    );
    assert_eq!(
        f.query(&["trace", "file:lib.sh", "--depth", "bad", "--no-refresh"])
            .0,
        2
    );
    for limit in ["0", "2001", "bad"] {
        assert_eq!(
            f.query(&["api", "lib.sh", "--limit", limit, "--no-refresh"])
                .0,
            2
        );
    }
    for limit in ["1", "2000"] {
        assert_eq!(
            f.query(&["api", "lib.sh", "--limit", limit, "--no-refresh"])
                .0,
            0
        );
    }
    assert_eq!(f.query(&["symbol", ""]).0, 2);
    assert_eq!(f.query(&["symbol", &"x".repeat(512)]).0, 0);
    assert_eq!(f.query(&["symbol", &"x".repeat(513)]).0, 2);
    for limit in ["0", "501", "bad"] {
        assert_eq!(f.query(&["symbol", "parseRecord", "--limit", limit]).0, 2);
    }
    for limit in ["1", "500"] {
        assert_eq!(f.query(&["symbol", "parseRecord", "--limit", limit]).0, 0);
    }
    let mut payload = project::load(&f.state()).unwrap();
    for node in payload["nodes"].as_array_mut().unwrap() {
        node["encoding_probe"] = json!({"z":[1e-7,1e20,1e16,-0.0,true,null,{},[]],"a":"한국 ß \t\n\u{0008}","escaped":"<&>"});
    }
    fs::write(f.state().join("graph.json"), payload.to_string()).unwrap();
    for pretty in [false, true] {
        let mut args: Vec<OsString> = ["project", "find", "parse", "--json", "--no-refresh"]
            .iter()
            .map(OsString::from)
            .collect();
        if pretty {
            args.push("--pretty".into());
        }
        let (code, out, err) = run(&args, &f.0, &cancel);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        let reference =
            oms_command_operations::value::V::parse(&serde_json::to_vec(&parsed).unwrap()).unwrap();
        let mut expected = reference.json(pretty).unwrap();
        if !pretty {
            expected.push(b'\n');
        }
        assert_eq!(out, expected, "canonical JSON pretty={pretty}");
    }
}
fn snapshot(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, paths: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            paths.push(entry.path());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), paths);
            }
        }
    }
    let mut paths = vec![];
    walk(root, &mut paths);
    paths.sort();
    paths
}

#[test]
fn compact_views_share_payloads_and_preserve_ordered_duplicate_edges() {
    check_lexical_reference();
    let value = json!({"nodes":[{"id":"z","kind":"file","path":"a.sh"},{"id":"a","kind":"function","path":"a.sh"},{"id":"test:a.sh","kind":"file","path":"a.sh"}],"edges":[{"source":"z","target":"a","relation":"calls","confidence":"EXTRACTED"},{"source":"z","target":"a","relation":"calls","confidence":"AMBIGUOUS"},{"source":"z","target":"a","relation":"contains"},{"source":"missing","target":"a","relation":"calls"}]});
    let graph = Graph::new(value).unwrap();
    assert!(
        std::ptr::eq(graph.node("a").unwrap(), &graph.graph["nodes"][1]),
        "node payload must be interned"
    );
    assert!(
        std::ptr::eq(
            graph.out.get("z").into_iter().flatten().next().unwrap(),
            &graph.graph["edges"][0]
        ),
        "adjacency must borrow the shared edge"
    );
    assert_eq!(
        graph.nodes.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a", "test:a.sh", "z"]
    );
    assert!(graph.is_test("a"));
    assert!(!graph.nodes.contains_key("missing"));
    assert_eq!(
        graph
            .out
            .get("z")
            .into_iter()
            .flatten()
            .map(|e| e["relation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["calls", "calls", "contains"]
    );
    assert_eq!(graph.ins.get("a").unwrap().len(), 4);
    assert_eq!(
        graph.trace("z", "out", 1, &[], 1, 0).unwrap()["omitted_edges"],
        3
    );
    assert_eq!(
        graph.trace("z", "out", 1, &[], 2, 3).unwrap()["truncated"],
        false
    );
    assert!(Graph::new(json!({"nodes":[{}],"edges":[]})).is_err());
    assert!(Graph::new(json!({"nodes":[],"edges":[{}]})).is_err());
    assert!(
        Graph::new(json!({"nodes":[],"edges":[]}))
            .unwrap()
            .find("", "", 0, false)
            .is_empty()
    );
}

fn check_lexical_reference() {
    let fixture: Value =
        serde_json::from_str(include_str!("project-engine-reference.json")).unwrap();
    let graph = Graph::new(fixture["graph"].clone()).unwrap();
    for case in fixture["find"].as_array().unwrap() {
        let rows = graph.find(
            case["query"].as_str().unwrap(),
            case["kind"].as_str().unwrap(),
            case["limit"].as_i64().unwrap(),
            case["include_tests"].as_bool().unwrap(),
        );
        let actual: Vec<Value> = rows.iter().map(|r| json!([r["id"], r["score"]])).collect();
        assert_eq!(json!(actual), case["rows"], "find reference {case}");
    }
    let f = Fixture::new();
    for (path, text) in fixture["sources"].as_object().unwrap() {
        let target = f.0.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, text.as_str().unwrap()).unwrap();
    }
    for case in fixture["context"].as_array().unwrap() {
        let actual = project::context::select(
            &f.0,
            &graph,
            case["task"].as_str().unwrap(),
            12,
            &[],
            "",
            &json!({}),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(actual["entries"], case["entries"], "BM25 entries {case}");
        assert_eq!(actual["evidence"], case["evidence"], "BM25 scores {case}");
    }
}

#[test]
fn rust_workspace_cache_and_impact() {
    let f = Fixture::new();
    for (path, contents) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"core\"]\n"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n[dependencies]\ncore = { path = \"../core\" }\n",
        ),
        ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
        ("app/src/lib.rs", "pub fn app() {}\n"),
        ("core/src/lib.rs", "pub fn core() {}\n"),
        ("app/tests/check.rs", "#[test]\nfn check() {}\n"),
    ] {
        let target = f.0.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, contents).unwrap();
    }
    fs::create_dir_all(f.state().join("cache")).unwrap();
    fs::write(f.state().join("cache/old.json"), "{}").unwrap();
    f.build();
    assert!(f.state().join("cache.sqlite3").is_file());
    assert!(!f.state().join("cache").exists());
    assert_eq!(
        fs::metadata(f.state().join("cache.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let graph = project::load(&f.state()).unwrap();
    assert!(
        graph["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["source"] == "file:app/Cargo.toml" && e["target"] == "file:core/src/lib.rs")
    );
    assert!(
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "check" && n["metadata"]["test"] == true)
    );
    let warm = f.build();
    assert!(warm["stats"]["cached"].as_u64().unwrap() > 0);
    assert_eq!(warm["stats"]["parsed"], 0);
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&f.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    let base = git(&["write-tree"]);
    fs::write(f.0.join("core/src/lib.rs"), "pub fn changed() {}\n").unwrap();
    git(&["add", "core/src/lib.rs"]);
    let head = git(&["write-tree"]);
    let (code, affected) = f.query(&[
        "affected",
        "--base",
        &base,
        "--head",
        &head,
        "--depth",
        "8",
        "--runnable",
        "*.rs",
    ]);
    assert_eq!(code, 0, "{affected}");
    assert!(
        affected.to_string().contains("app/tests/check.rs"),
        "{affected}"
    );
}
