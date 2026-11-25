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
    fn empty() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oms-project-engine-empty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
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
    // Keep additive source contracts intact in both query backends. The
    // extraction/annotation semantics are exercised by graph-python's cases.
    let contract = json!({"basis":"declared_source","parameters":[{"name":"record","kind":"positional_or_keyword","annotation":"Record","line":2,"has_default":false}],"returns":"bool","evidence":{"path":"lib.sh","source_digest":"fixture","line":2}});
    let symbol = payload["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["id"] == "symbol:lib.sh::parseRecord")
        .unwrap();
    symbol["metadata"]["callable_contract"] = contract.clone();
    db.execute(
        "UPDATE nodes SET data=?1 WHERE id=?2",
        rusqlite::params![symbol.to_string(), "symbol:lib.sh::parseRecord"],
    )
    .unwrap();
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
    assert_eq!(
        graph.file_api("lib.sh", 500).unwrap()["symbols"][0]["callable_contract"],
        contract
    );
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
fn signatures_keep_later_ranked_files_after_a_locally_clipped_declaration() {
    let f = Fixture::new();
    let long_type = "Argument".repeat(300);
    fs::write(
        f.0.join("aaa.rs"),
        format!("pub fn clipped(value: {long_type}) {{}}\n"),
    )
    .unwrap();
    fs::write(f.0.join("zzz.rs"), "pub fn later() {}\n").unwrap();
    f.build();

    let query = |budget| {
        let (code, report) = f.query(&[
            "map",
            "--signatures",
            "--no-refresh",
            "--entry-path",
            "aaa.rs",
            "--entry-path",
            "zzz.rs",
            "--max-files",
            "2",
            "--max-bytes",
            budget,
        ]);
        assert_eq!(code, 0, "{report}");
        report
    };
    let wide = query("4096");
    assert_eq!(wide["truncated"], json!(["aaa.rs"]));
    assert_eq!(wide["omitted"], json!([]));
    assert_eq!(wide["files"][0]["path"], "aaa.rs");
    assert_eq!(wide["files"][1]["path"], "zzz.rs");
    assert!(wide["text"].as_str().unwrap().contains("pub fn later()"));

    let narrow = query("256");
    assert_eq!(narrow["truncated"], json!(["aaa.rs"]));
    assert_eq!(narrow["omitted"], json!(["zzz.rs"]));
    assert!(narrow["bytes"].as_u64().unwrap() <= 256);
}

#[test]
fn rust_workspace_cache_and_impact() {
    let f = Fixture::new();
    for (path, contents) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"core\"]\n"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n[dependencies]\napp_core = { package = \"core\", path = \"../core\" }\n",
        ),
        ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
        ("app/src/lib.rs", "pub fn app() {}\n"),
        (
            "app/src/chain.rs",
            "pub fn run() { run_with_source() }\nfn run_with_source() { run_with_services(&Unavailable) }\nfn run_with_services(_: &Unavailable) {}\nfn elsewhere() { core() }\nstruct Unavailable;\nuse app_core::core;\n",
        ),
        (
            "core/src/lib.rs",
            "pub fn core() {}\nimpl First { pub fn new() -> Self { Self } }\nimpl Second { pub fn new() -> Self { Self } }\n",
        ),
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
    let (code, api) = f.query(&["api", "core/src/lib.rs", "--no-refresh"]);
    assert_eq!(code, 0);
    let methods: Vec<_> = api["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["name"] == "new")
        .collect();
    assert_eq!(methods.len(), 2);
    assert_ne!(methods[0]["id"], methods[1]["id"]);
    assert!(
        methods
            .iter()
            .all(|s| s["signature"].as_str().unwrap().contains("fn new()"))
    );
    // `core` exists only in another file, so same-name resolution must not
    // turn `elsewhere` into a caller.
    let calls = |graph: &Value| -> Vec<Value> {
        graph["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["relation"] == "calls"
                    && e["source"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("symbol:app/src/chain.rs::"))
            })
            .cloned()
            .collect()
    };
    let cold_calls = calls(&graph);
    let id = |name: &str| format!("symbol:app/src/chain.rs::function:{name}");
    let digest = &graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id("run").as_str())
        .unwrap()["source_digest"];
    let expected: Vec<Value> = [
        ("run", "run_with_source", 1),
        ("run_with_source", "run_with_services", 2),
    ]
    .iter()
    .map(|(source, target, line)| {
        json!({"source":id(source),"target":id(target),"relation":"calls","confidence":"INFERRED","evidence":{"path":"app/src/chain.rs","source_digest":digest,"line":line}})
    })
    .collect();
    assert_eq!(cold_calls, expected);
    for (direction, other) in [("in", "run"), ("out", "run_with_services")] {
        assert_eq!(
            f.graph()
                .neighbors(&id("run_with_source"), "calls", direction)
                .unwrap(),
            vec![
                json!({"id":id(other),"relation":"calls","direction":direction,"confidence":"INFERRED"})
            ]
        );
    }
    let warm = f.build();
    assert!(warm["stats"]["cached"].as_u64().unwrap() > 0);
    assert_eq!(warm["stats"]["parsed"], 0);
    assert_eq!(calls(&project::load(&f.state()).unwrap()), cold_calls);
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

#[test]
fn panel_interrupt_during_selected_python_request_returns_no_output() {
    use sha2::{Digest, Sha256};
    use std::process::Command;

    let repo = Fixture::new();
    repo.build();
    let path = "focus.py";
    let source = "def focus():\n    pass\n";
    fs::write(repo.0.join(path), source).unwrap();
    for args in [["init", "-q"], ["add", path]] {
        assert!(
            Command::new("git")
                .current_dir(&repo.0)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    // Model a selection from a previously indexed Python file without
    // requiring the optional packaged AST binary to build the test index.
    let mut graph = project::load(&repo.state()).unwrap();
    let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
    let nodes = graph["nodes"].as_array_mut().unwrap();
    nodes.push(json!({"id":"file:focus.py","kind":"file","path":path,"source_digest":digest}));
    nodes.push(json!({"id":"symbol:focus.py::focus","kind":"function","name":"focus","path":path,
        "language":"python","source_digest":digest,"metadata":{"signature":"def focus()","line":1,"end_line":2}}));
    fs::write(repo.state().join("graph.json"), graph.to_string()).unwrap();

    let private = Fixture::empty();
    let helper = private.0.join("parser");
    let ready = private.0.join("parser.ready");
    assert!(
        Command::new("mkfifo")
            .arg(&ready)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        &helper,
        br#"#!/bin/sh
[ "$(dd bs=1 count=6 2>/dev/null)" = "$(printf 'OGPY\002\001')" ] || exit 1
printf 'request\n' > "$0.ready"
exec cat >/dev/null
"#,
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let args = [
        "project",
        "panel",
        "--repo",
        ".",
        "--node=symbol:focus.py::focus",
        "--json",
    ]
    .map(OsString::from);
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let call = scope
            .spawn(|| oms_command_graph::run_with_helper(&args, &repo.0, &cancel, Some(&helper)));
        // Opening both ends makes the start deadline apply even if the helper
        // was never invoked. The helper blocks until the session cancels it.
        let started = Command::new("bash")
            .args([
                "-c",
                r#"exec 3<>"$1"; IFS= read -r -t 8 -u 3 marker && [[ $marker = request ]]"#,
                "wait",
            ])
            .arg(&ready)
            .status();
        cancel.store(true, Ordering::SeqCst);
        let (code, stdout, stderr) = call.join().unwrap();
        assert!(
            started.unwrap().success(),
            "parser did not receive its request"
        );
        assert_eq!(code, 130);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"graph: interrupted\n");
    });
}

#[test]
fn panel_projection_covers_unindexed_indexed_bounds_and_no_writes() {
    fn tree_fingerprint(root: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((rel, meta.len(), mtime));
                if meta.is_dir() && !meta.file_type().is_symlink() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out
    }
    let panel_with_helper =
        |repo: &Path, extra: &[&str], helper: Option<&Path>| -> (i32, Value, String) {
            let mut args: Vec<OsString> = vec![
                "project".into(),
                "panel".into(),
                "--repo".into(),
                repo.as_os_str().to_owned(),
                "--json".into(),
            ];
            args.extend(extra.iter().map(OsString::from));
            let (code, out, err) =
                oms_command_graph::run_with_helper(&args, repo, &AtomicBool::new(false), helper);
            let stderr = String::from_utf8_lossy(&err).into_owned();
            let value = if out.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&out).unwrap_or_else(|e| {
                    panic!(
                        "invalid panel JSON ({e}): stdout={} stderr={stderr}",
                        String::from_utf8_lossy(&out)
                    )
                })
            };
            (code, value, stderr)
        };
    let run_panel = |repo: &Path, extra: &[&str]| panel_with_helper(repo, extra, None);
    let git = |repo: &Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };

    // 1. Invalid project states: plain dir (even nested under a parent git repo), broken .git file, symlinked .git.
    let outer = Fixture::empty();
    git(&outer.0, &["init", "-q"]);
    let plain = outer.0.join("not-a-repo");
    fs::create_dir(&plain).unwrap();
    fs::write(plain.join("file.rs"), "pub fn x() {}\n").unwrap();
    let before_plain = tree_fingerprint(&plain);
    let (code, _, stderr) = run_panel(&plain, &[]);
    assert_eq!(code, 2, "plain dir must fail closed, stderr={stderr}");
    assert!(stderr.contains("invalid project repository"), "{stderr}");
    assert_eq!(tree_fingerprint(&plain), before_plain);

    let broken_git = outer.0.join("broken-git");
    fs::create_dir(&broken_git).unwrap();
    fs::write(broken_git.join(".git"), "gitdir: /nonexistent/outside\n").unwrap();
    let (code, _, stderr) = run_panel(&broken_git, &[]);
    assert_eq!(code, 2, "broken .git must fail closed, stderr={stderr}");
    assert!(stderr.contains("invalid project repository"), "{stderr}");

    let symlink_git = outer.0.join("symlink-git");
    fs::create_dir(&symlink_git).unwrap();
    std::os::unix::fs::symlink(outer.0.join(".git"), symlink_git.join(".git")).unwrap();
    let (code, _, stderr) = run_panel(&symlink_git, &[]);
    assert_eq!(code, 2, "symlink .git must fail closed, stderr={stderr}");
    assert!(stderr.contains("invalid project repository"), "{stderr}");

    // 2. Empty git repository -> status=empty, no writes.
    let empty_repo = Fixture::empty();
    git(&empty_repo.0, &["init", "-q"]);
    let before_empty = tree_fingerprint(&empty_repo.0);
    let (code, empty_dto, stderr) = run_panel(&empty_repo.0, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(empty_dto["schema"], 1);
    assert_eq!(empty_dto["kind"], "oms-panel-project-graph");
    assert_eq!(empty_dto["status"], "empty");
    assert_eq!(empty_dto["completeness"], "complete");
    assert_eq!(empty_dto["provenance"]["source"], "git-ls-files");
    assert_eq!(empty_dto["provenance"]["mode"], "fallback");
    assert_eq!(empty_dto["provenance"]["indexState"], "unindexed");
    assert_eq!(empty_dto["provenance"]["wroteIndex"], false);
    assert_eq!(empty_dto["nodes"], json!([]));
    assert_eq!(empty_dto["edges"], json!([]));
    assert!(!empty_repo.0.join(".oms").exists());
    assert_eq!(tree_fingerprint(&empty_repo.0), before_empty);

    // 3. Genuine small unindexed git repo: real structure, secret/symlink filtering, focus/drilldown, query, bounds, zero writes.
    let unindexed = Fixture::empty();
    git(&unindexed.0, &["init", "-q"]);
    fs::create_dir_all(unindexed.0.join("src/sub")).unwrap();
    fs::create_dir_all(unindexed.0.join("tests")).unwrap();
    fs::create_dir_all(unindexed.0.join("keys")).unwrap();
    fs::write(unindexed.0.join("README.md"), "# Demo\n").unwrap();
    fs::write(
        unindexed.0.join("src/lib.rs"),
        "mod util;\npub fn entry() -> i32 { util::helper() }\n",
    )
    .unwrap();
    fs::write(
        unindexed.0.join("src/util.rs"),
        "pub fn helper() -> i32 { 42 }\n",
    )
    .unwrap();
    fs::write(unindexed.0.join("src/sub/deep.rs"), "pub fn deep() {}\n").unwrap();
    fs::write(
        unindexed.0.join("tests/basic_test.rs"),
        "#[test]\nfn basic() { let _ = \"src/lib.rs\"; }\n",
    )
    .unwrap();
    // Secret-bearing files must never appear in fallback output or be read.
    fs::write(
        unindexed.0.join(".env"),
        "TOP_SECRET_VALUE=should_never_leak\n",
    )
    .unwrap();
    fs::write(unindexed.0.join("keys/private.pem"), "pem-body\n").unwrap();
    let outside = outer.0.join("outside.txt");
    fs::write(&outside, "outside-secret\n").unwrap();
    std::os::unix::fs::symlink(&outside, unindexed.0.join("src/escape.rs")).unwrap();
    git(
        &unindexed.0,
        &[
            "add",
            "README.md",
            "src/lib.rs",
            "src/util.rs",
            "src/sub/deep.rs",
            "tests/basic_test.rs",
        ],
    );

    git(
        &unindexed.0,
        &["add", "-f", ".env", "keys/private.pem", "src/escape.rs"],
    );
    fs::write(
        unindexed.0.join("src/untracked_sentinel.rs"),
        "fn untracked_sentinel() {}",
    )
    .unwrap();
    fs::write(unindexed.0.join("src/문서 파일.rs"), "fn demo() {}").unwrap();
    git(&unindexed.0, &["add", "src/문서 파일.rs"]);
    fs::write(
        unindexed.0.join(".git/info/exclude"),
        "ignored_sentinel.rs\n",
    )
    .unwrap();
    fs::write(unindexed.0.join("ignored_sentinel.rs"), "fn demo() {}").unwrap();
    let monitor = outer.0.join("monitor");
    fs::write(&monitor, "#!/bin/sh\ntouch invoked-hook\n").unwrap();
    fs::set_permissions(&monitor, fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &unindexed.0,
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );
    let before_unindexed = tree_fingerprint(&unindexed.0);
    let (code, dto, stderr) = run_panel(&unindexed.0, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(dto["status"], "ready");
    // `src/sub/deep.rs` is at depth 3 while default depth is 2, so first-load marks dir:src/sub expandable.
    assert_eq!(dto["completeness"], "partial");
    assert_eq!(dto["provenance"]["source"], "git-ls-files");
    assert_eq!(dto["provenance"]["mode"], "fallback");
    assert_eq!(dto["provenance"]["indexState"], "unindexed");
    assert_eq!(
        dto["provenance"]["relationshipBasis"],
        "structural-containment"
    );
    assert_eq!(dto["provenance"]["verifiedRelationships"], false);
    assert_eq!(dto["provenance"]["readOnly"], true);
    assert_eq!(dto["provenance"]["wroteIndex"], false);
    // Scope repository is basename only, never absolute path.
    assert!(!dto["scope"]["repository"].as_str().unwrap().contains('/'));
    assert!(!dto.to_string().contains("untracked_sentinel"));
    assert!(!dto.to_string().contains("ignored_sentinel"));
    assert_eq!(
        run_panel(&unindexed.0, &["--node=file:src/문서 파일.rs"]).1["selection"]["status"],
        "found"
    );
    assert!(!unindexed.0.join("invoked-hook").exists());
    assert!(!dto.to_string().contains(".env"));
    assert!(!dto.to_string().contains("private.pem"));
    assert!(!dto.to_string().contains("escape.rs"));
    assert!(!dto.to_string().contains("should_never_leak"));
    assert!(!dto.to_string().contains("outside-secret"));
    assert!(dto["limits"]["excludedPaths"].as_u64().unwrap() >= 3);
    assert_eq!(dto["counts"]["hidden_test_nodes"], 1);
    let node_ids: Vec<&str> = dto["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(node_ids.contains(&"dir:."));
    assert!(node_ids.contains(&"dir:src"));
    assert!(node_ids.contains(&"dir:src/sub"));
    assert!(node_ids.contains(&"file:README.md"));
    assert!(node_ids.contains(&"file:src/lib.rs"));
    assert!(!node_ids.contains(&"test:tests/basic_test.rs"));
    let sub_node = dto["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "dir:src/sub")
        .unwrap();
    assert_eq!(sub_node["expandable"], true);
    assert_eq!(sub_node["omitted_children"], 1);
    let (code, full_depth, _) = run_panel(&unindexed.0, &["--depth", "3"]);
    assert_eq!(code, 0);
    assert_eq!(full_depth["completeness"], "complete");
    assert!(
        full_depth["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "file:src/sub/deep.rs")
    );
    let edges = dto["edges"].as_array().unwrap();
    assert!(!edges.is_empty());
    assert!(
        edges
            .iter()
            .all(|e| e["relation"] == "contains" && e["confidence"] == "STRUCTURAL")
    );

    // Include tests reveals test file.
    let (code, with_tests, _) = run_panel(&unindexed.0, &["--include-tests"]);
    assert_eq!(code, 0);
    assert!(
        with_tests["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "test:tests/basic_test.rs")
    );

    // Focus / drilldown on directory node.
    let (code, dir_focus, _) = run_panel(&unindexed.0, &["--node=dir:src", "--depth", "1"]);
    assert_eq!(code, 0);
    assert_eq!(dir_focus["selection"]["status"], "found");
    assert_eq!(dir_focus["selection"]["node"]["id"], "dir:src");
    assert_eq!(
        dir_focus["selection"]["breadcrumbs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["dir:.", "dir:src"]
    );
    let neighbor_ids: Vec<&str> = dir_focus["selection"]["neighbors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(neighbor_ids.contains(&"dir:."));
    assert!(neighbor_ids.contains(&"dir:src/sub"));
    assert!(neighbor_ids.contains(&"file:src/lib.rs"));
    assert!(neighbor_ids.contains(&"file:src/util.rs"));

    // Focus on file node.
    let (code, file_focus, _) = run_panel(&unindexed.0, &["--node=file:src/lib.rs"]);
    assert_eq!(code, 0);
    assert_eq!(file_focus["selection"]["status"], "found");
    assert_eq!(file_focus["selection"]["node"]["id"], "file:src/lib.rs");
    assert_eq!(file_focus["selection"]["neighbors"][0]["id"], "dir:src");

    // Missing node -> clean not_found state.
    let (code, missing, _) = run_panel(&unindexed.0, &["--node=dir:does-not-exist"]);
    assert_eq!(code, 0);
    assert_eq!(missing["status"], "not_found");
    assert_eq!(missing["selection"]["status"], "not_found");
    assert_eq!(missing["nodes"], json!([]));

    // Query match and empty query miss.
    let (code, qhit, _) = run_panel(&unindexed.0, &["--query=util"]);
    assert_eq!(code, 0);
    assert_eq!(qhit["status"], "ready");
    assert!(
        qhit["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "file:src/util.rs")
    );
    let (code, qmiss, _) = run_panel(&unindexed.0, &["--query=nonexistent_xyz_token"]);
    assert_eq!(code, 0);
    assert_eq!(qmiss["status"], "empty");
    assert_eq!(qmiss["nodes"], json!([]));

    // Scale cap & bounds enforcement.
    let (code, capped, _) = run_panel(&unindexed.0, &["--limit", "3", "--depth", "1"]);
    assert_eq!(code, 0);
    assert!(capped["nodes"].as_array().unwrap().len() <= 3);
    assert_eq!(capped["limits"]["truncated"], true);
    assert_eq!(capped["completeness"], "partial");

    for bad_args in [
        &["--limit", "0"][..],
        &["--limit", "61"][..],
        &["--depth", "4"][..],
        &["--node=dir:src", "--query=src"][..],
        &["--node=file:../etc/passwd"][..],
        &["--node=file:/etc/passwd"][..],
    ] {
        let (code, _, _) = run_panel(&unindexed.0, bad_args);
        assert_eq!(code, 2, "expected refusal for {bad_args:?}");
    }

    // Strictly zero writes during all unindexed reads.
    assert!(!unindexed.0.join(".oms").exists());
    assert_eq!(tree_fingerprint(&unindexed.0), before_unindexed);

    // A non-Git checkout cannot authorize a tracked inventory.
    let oms_only = Fixture::empty();
    fs::create_dir_all(oms_only.0.join(".oms")).unwrap();
    fs::create_dir_all(oms_only.0.join("pkg")).unwrap();
    fs::write(oms_only.0.join("pkg/app.rs"), "pub fn app() {}\n").unwrap();
    let before_oms_only = tree_fingerprint(&oms_only.0);
    let (code, oms_dto, stderr) = run_panel(&oms_only.0, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(oms_dto["status"], "unknown");
    assert_eq!(oms_dto["nodes"], json!([]));
    assert_eq!(tree_fingerprint(&oms_only.0), before_oms_only);

    // 5. Corrupt index falls back cleanly to partial state without rewriting index.
    let corrupt_dir = unindexed.0.join(".oms/project-graph");
    fs::create_dir_all(&corrupt_dir).unwrap();
    fs::write(corrupt_dir.join("graph.json"), b"{not valid json").unwrap();
    let before_corrupt = tree_fingerprint(&unindexed.0);
    let (code, corrupt_dto, stderr) = run_panel(&unindexed.0, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(corrupt_dto["status"], "partial");
    assert_eq!(corrupt_dto["completeness"], "partial");
    assert_eq!(corrupt_dto["provenance"]["mode"], "fallback");
    assert_eq!(corrupt_dto["provenance"]["indexState"], "invalid");
    assert_eq!(corrupt_dto["provenance"]["wroteIndex"], false);
    assert_eq!(tree_fingerprint(&unindexed.0), before_corrupt);
    fs::remove_dir_all(unindexed.0.join(".oms")).unwrap();

    // 6. Genuine indexed repository: uses cached index with verified relationships and zero writes.
    let indexed = Fixture::empty();
    git(&indexed.0, &["init", "-q"]);
    fs::create_dir_all(indexed.0.join("src")).unwrap();
    fs::create_dir_all(indexed.0.join("tests")).unwrap();
    fs::set_permissions(indexed.0.join("src"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(indexed.0.join("tests"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        indexed.0.join("src/lib.sh"),
        "#!/usr/bin/env bash\nhelper() { echo ok; }\n",
    )
    .unwrap();
    fs::write(
        indexed.0.join("src/main.sh"),
        "#!/usr/bin/env bash\nsource src/lib.sh\nrun_app() { helper; }\n",
    )
    .unwrap();
    fs::write(
        indexed.0.join("tests/test-app.sh"),
        "#!/usr/bin/env bash\nsource src/main.sh\nrun_app\n",
    )
    .unwrap();
    fs::write(
        indexed.0.join("src/record.rs"),
        "pub fn inspect(value: usize) -> usize {\n    value\n}\nfn measured(_: [u8; \"literal\".len()]) {}\nfn replacement() {}\nmod left {\n    pub fn scoped() {}\n}\nmod right {\n    pub fn scoped() {}\n}\n",
    )
    .unwrap();
    for path in [
        "src/lib.sh",
        "src/main.sh",
        "src/record.rs",
        "tests/test-app.sh",
    ] {
        fs::set_permissions(indexed.0.join(path), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let helper_bin = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("oms-graph-python-helper");
    let helper_bin = helper_bin.is_file().then_some(helper_bin.as_path());
    if helper_bin.is_some() {
        fs::write(indexed.0.join("src/inspect.py"), "def focus():\n    pass\n").unwrap();
    }
    git(&indexed.0, &["add", "."]);
    fs::write(
        indexed.0.join("src/untracked_sentinel.sh"),
        "untracked_sentinel() { echo ok; }",
    )
    .unwrap();
    let built = project::build(
        &indexed.0,
        &indexed.state(),
        &Default::default(),
        &AtomicBool::new(false),
        helper_bin,
    )
    .unwrap();
    assert!(built["stats"]["nodes"].as_u64().unwrap() > 0);
    let before_indexed = tree_fingerprint(&indexed.0);

    let (code, idx_dto, stderr) = run_panel(&indexed.0, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!idx_dto.to_string().contains("untracked_sentinel"));
    assert_eq!(idx_dto["status"], "ready");
    assert_eq!(idx_dto["provenance"]["source"], "indexed-graph");
    assert_eq!(idx_dto["provenance"]["mode"], "indexed");
    assert_eq!(idx_dto["provenance"]["indexState"], "present");
    assert_eq!(idx_dto["provenance"]["indexFreshness"], "cached");
    assert_eq!(idx_dto["provenance"]["verifiedRelationships"], true);
    assert_eq!(
        idx_dto["provenance"]["relationshipBasis"],
        "verified-and-inferred"
    );
    assert_eq!(idx_dto["provenance"]["wroteIndex"], false);
    assert!(
        idx_dto["counts"]["confidence"]["EXTRACTED"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "expected verified EXTRACTED edges: {idx_dto}"
    );

    // Indexed node focus & neighbors.
    let (code, idx_focus, _) = run_panel(&indexed.0, &["--node=file:src/main.sh"]);
    assert_eq!(code, 0);
    assert_eq!(idx_focus["selection"]["status"], "found");
    assert_eq!(idx_focus["selection"]["node"]["id"], "file:src/main.sh");
    assert!(
        idx_focus["selection"]["neighbors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["confidence"] == "EXTRACTED"),
        "expected EXTRACTED neighbor: {idx_focus}"
    );

    if let Some(parser) = helper_bin {
        let args = ["--node=symbol:src/inspect.py::focus"];
        let (_, python, _) = panel_with_helper(&indexed.0, &args, Some(parser));
        assert_eq!(
            python["selection"]["definition"]["signature"],
            "def focus()"
        );
        assert_eq!(python["selection"]["definition"]["endLine"], 2);
        let (_, unavailable, _) = run_panel(&indexed.0, &args);
        assert_eq!(unavailable["selection"]["status"], "found");
        assert!(unavailable["selection"].get("definition").is_none());
    }
    let helper_id = "--node=symbol:src/lib.sh::helper";
    let (_, helper, _) = run_panel(&indexed.0, &[helper_id]);
    let detail = &helper["selection"]["definition"];
    assert_eq!(detail["id"], "symbol:src/lib.sh::helper");
    assert_eq!(detail["signature"], "helper()");
    assert_eq!(
        (&detail["line"], &detail["endLine"]),
        (&json!(2), &json!(2))
    );
    assert!(!helper["nodes"].to_string().contains("helper()"));
    assert!(idx_focus["selection"].get("definition").is_none());
    let rust_id = "--node=symbol:src/record.rs::function:inspect";
    let (_, rust, _) = run_panel(&indexed.0, &[rust_id]);
    let detail = &rust["selection"]["definition"];
    assert_eq!(detail["signature"], "pub fn inspect(value: usize) -> usize");
    assert_eq!(
        (&detail["line"], &detail["endLine"]),
        (&json!(1), &json!(3))
    );
    let (_, literal, _) = run_panel(
        &indexed.0,
        &["--node=symbol:src/record.rs::function:measured"],
    );
    assert_eq!(literal["selection"]["status"], "found");
    assert!(literal["selection"].get("definition").is_none());
    assert!(!literal.to_string().contains("literal"));
    // Indexed directory drilldown and missing node.
    let (code, idx_dir, _) = run_panel(&indexed.0, &["--node=dir:src"]);
    assert_eq!(code, 0);
    assert_eq!(idx_dir["selection"]["status"], "found");
    assert_eq!(idx_dir["selection"]["node"]["kind"], "directory");
    let (code, idx_missing, _) = run_panel(&indexed.0, &["--node=symbol:src/main.sh::nope"]);
    assert_eq!(code, 0);
    assert_eq!(idx_missing["status"], "not_found");
    assert_eq!(idx_missing["selection"]["status"], "not_found");

    // Indexed query.
    let (code, idx_query, _) = run_panel(&indexed.0, &["--query=helper"]);
    assert_eq!(code, 0);
    assert_eq!(idx_query["status"], "ready");
    assert!(
        idx_query["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["label"] == "helper" || n["path"] == "src/lib.sh")
    );

    // Assert zero writes or mtime changes to the index or repo.
    assert_eq!(tree_fingerprint(&indexed.0), before_indexed);

    // Even with unchanged bytes, a cached declaration can point at another
    // real function, including an identical spelling in a different scope.
    let graph_path = indexed.state().join("graph.json");
    let original = fs::read(&graph_path).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    let nodes = changed["nodes"].as_array_mut().unwrap();
    let index_of = |local: &str| {
        let id = format!("symbol:src/record.rs::function:{local}");
        nodes.iter().position(|n| n["id"] == id).unwrap()
    };
    let (inspect, replacement) = (index_of("inspect"), index_of("replacement"));
    let (left, right) = (index_of("left::scoped"), index_of("right::scoped"));
    let left_arg = "--node=symbol:src/record.rs::function:left::scoped";
    let (_, scoped, _) = run_panel(&indexed.0, &[left_arg]);
    assert_eq!(scoped["selection"]["definition"]["line"], 7);
    for key in ["name", "metadata"] {
        nodes[inspect][key] = nodes[replacement][key].clone();
    }
    for key in ["line", "end_line"] {
        nodes[left]["metadata"][key] = nodes[right]["metadata"][key].clone();
    }
    fs::write(&graph_path, changed.to_string()).unwrap();
    for arg in [rust_id, left_arg] {
        let (_, changed, _) = run_panel(&indexed.0, &[arg]);
        assert_eq!(changed["selection"]["status"], "found");
        assert!(changed["selection"].get("definition").is_none(), "{arg}");
    }
    fs::write(&graph_path, original).unwrap();

    // Matching manifest versions cannot authorize text from an edited file.
    fs::write(indexed.0.join("src/lib.sh"), "helper() { echo changed; }\n").unwrap();
    let (_, edited, _) = run_panel(&indexed.0, &[helper_id]);
    assert_eq!(edited["selection"]["status"], "found");
    assert!(edited["selection"].get("definition").is_none());

    let mut stale: Value =
        serde_json::from_slice(&fs::read(indexed.state().join("manifest.json")).unwrap()).unwrap();
    stale["parser_version"] = json!(0);
    indexed.manifest(&stale);
    let (_, stale, _) = run_panel(&indexed.0, &[rust_id]);
    assert_eq!(stale["provenance"]["indexFreshness"], "stale");
    assert!(stale["selection"].get("definition").is_none());

    // 7. Large repository scale cap keeps payload bounded under MAX_RESPONSE_BYTES.
    let large = Fixture::empty();
    git(&large.0, &["init", "-q"]);
    for d in 0..15 {
        let sub = large.0.join(format!("mod_{d:02}"));
        fs::create_dir(&sub).unwrap();
        for f in 0..6 {
            fs::write(sub.join(format!("item_{f:02}.rs")), "pub fn f() {}\n").unwrap();
        }
    }
    git(&large.0, &["add", "."]);
    let before_large = tree_fingerprint(&large.0);
    let (code, large_dto, stderr) = run_panel(&large.0, &["--limit", "60", "--depth", "3"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(large_dto["nodes"].as_array().unwrap().len() <= 60);
    assert!(large_dto["edges"].as_array().unwrap().len() <= 180);
    assert!(large_dto.to_string().len() <= 52_000);
    assert_eq!(large_dto["limits"]["truncated"], true);
    assert_eq!(large_dto["completeness"], "partial");
    assert_eq!(tree_fingerprint(&large.0), before_large);
}

#[test]
fn panel_streams_indexes_between_sixteen_and_twenty_four_mib() {
    let repo = Fixture::empty();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    fs::create_dir_all(repo.0.join("src")).unwrap();
    fs::set_permissions(repo.0.join("src"), fs::Permissions::from_mode(0o700)).unwrap();
    for (path, text) in [
        ("src/lib.sh", "#!/usr/bin/env bash\nhelper() { echo ok; }\n"),
        (
            "src/main.sh",
            "#!/usr/bin/env bash\nsource src/lib.sh\nrun_app() { helper; }\n",
        ),
    ] {
        fs::write(repo.0.join(path), text).unwrap();
        fs::set_permissions(repo.0.join(path), fs::Permissions::from_mode(0o600)).unwrap();
    }
    git(&["add", "."]);
    fs::write(
        repo.0.join("src/untracked_sentinel.sh"),
        "untracked_sentinel() { echo ok; }\n",
    )
    .unwrap();
    project::build(
        &repo.0,
        &repo.state(),
        &Default::default(),
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    let panel = |extra: &[&str]| -> Value {
        let mut args: Vec<OsString> = ["project", "panel", "--repo"].map(Into::into).to_vec();
        args.push(repo.0.as_os_str().to_owned());
        args.push("--json".into());
        args.extend(extra.iter().map(OsString::from));
        let (code, out, err) =
            oms_command_graph::run_with_helper(&args, &repo.0, &AtomicBool::new(false), None);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        serde_json::from_slice(&out).unwrap()
    };
    let probes: [&[&str]; 3] = [
        &[],
        &["--query=helper"],
        &["--node=symbol:src/lib.sh::helper"],
    ];
    let small: Vec<Value> = probes.iter().map(|p| panel(p)).collect();
    assert_eq!(small[0]["provenance"]["mode"], "indexed");
    assert_eq!(small[2]["selection"]["status"], "found");
    assert!(small[2]["selection"]["definition"].is_object());

    let graph_path = repo.state().join("graph.json");
    let original = fs::read(&graph_path).unwrap();
    let base: Value = serde_json::from_slice(&original).unwrap();
    assert!(
        base["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["path"] == "src/untracked_sentinel.sh")
    );

    // The field lives inside a node: oversized rows still work when most of
    // the input is a discarded field. Fill once, without building a second DOM.
    let large = |graph: &Value| {
        let mut raw = serde_json::to_vec(graph).unwrap();
        let start = raw.windows(8).position(|w| w == b"ROW_PAD!").unwrap();
        raw.splice(start..start + 8, std::iter::repeat_n(b'x', 17 << 20));
        assert!(raw.len() > 16 << 20 && raw.len() < 24 << 20);
        raw
    };
    let mut fixture = base.clone();
    fixture["nodes"][0]["unused"] = json!("ROW_PAD!");
    fixture["unused_root"] = json!("ROOT_PAD");
    let tracked_file = "file:src/lib.sh";
    let untracked_file = "file:src/untracked_sentinel.sh";
    fixture["edges"].as_array_mut().unwrap().push(json!({
        "source":untracked_file, "target":tracked_file, "relation":"imports", "confidence":"EXTRACTED"
    }));
    let mut raw = large(&fixture);
    fs::write(&graph_path, &raw).unwrap();
    for (probe, expected) in probes.iter().zip(&small) {
        assert_eq!(&panel(probe), expected, "{probe:?}");
    }
    let missing = panel(&["--query=untracked_sentinel"]);
    assert_eq!(missing["provenance"]["mode"], "indexed");
    assert_eq!(missing["status"], "empty");
    let selected = panel(&["--node=file:src/lib.sh"]);
    assert_eq!(selected["selection"]["status"], "found");
    assert!(
        selected["selection"]["neighbors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["id"] != untracked_file)
    );
    assert_eq!(fs::read(&graph_path).unwrap(), raw);

    let mark = raw.windows(8).position(|w| w == b"ROOT_PAD").unwrap();
    for invalid in [
        br"\uD800xx".as_slice(),
        &[0xff, b'b', b'c', b'd', b'e', b'f', b'g', b'h'],
    ] {
        raw[mark..mark + 8].copy_from_slice(invalid);
        fs::write(&graph_path, &raw).unwrap();
        assert_eq!(panel(&[])["provenance"]["indexState"], "invalid");
    }
    raw[mark..mark + 8].copy_from_slice(b"ROOT_PAD");
    raw.truncate(raw.len() - 3);
    fs::write(&graph_path, &raw).unwrap();
    assert_eq!(panel(&[])["provenance"]["indexState"], "invalid");
    raw.resize((24 << 20) + 1, b' ');
    fs::write(&graph_path, &raw).unwrap();
    let over = panel(&[]);
    assert_eq!(over["provenance"]["mode"], "fallback");
    assert_eq!(over["provenance"]["indexState"], "invalid");
    drop(raw);

    let helper = fixture["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .position(|row| row["id"] == "symbol:src/lib.sh::helper")
        .unwrap();
    assert!(
        fixture["nodes"][helper]["metadata"]
            .get("scope_truncated")
            .is_none()
    );
    fixture["nodes"][helper]["metadata"]["scope_truncated"] = json!("wrong type");
    for bytes in [serde_json::to_vec(&fixture).unwrap(), large(&fixture)] {
        fs::write(&graph_path, bytes).unwrap();
        let dto = panel(probes[2]);
        assert_eq!(dto["provenance"]["mode"], "indexed");
        assert_eq!(dto["selection"]["status"], "found");
        assert!(dto["selection"].get("definition").is_none());
    }

    // The file fits the raw limit and its record count fits the node limit.
    // Individually valid text fields must still respect the aggregate search budget.
    for i in 0..1100 {
        fixture["nodes"].as_array_mut().unwrap().push(json!({
            "id":format!("symbol:src/lib.sh::node_{i}"),
            "path":"src/lib.sh", "summary":"word ".repeat(800)
        }));
    }
    fs::write(&graph_path, large(&fixture)).unwrap();
    let heavy = panel(&[]);
    assert_eq!(heavy["provenance"]["mode"], "fallback");
    assert_eq!(heavy["provenance"]["indexState"], "invalid");
    assert!(
        heavy["provenance"]["note"]
            .as_str()
            .unwrap()
            .contains("budget")
    );
}
