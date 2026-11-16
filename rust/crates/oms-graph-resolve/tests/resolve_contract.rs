use oms_graph_resolve::{Extraction, resolve};
use serde_json::{Value, json};
fn file(path: &str, parser: &str, nodes: Vec<Value>, refs: Vec<Value>) -> Extraction {
    Extraction {
        path: path.into(),
        sha256: "digest".into(),
        parser: parser.into(),
        nodes,
        edges: vec![],
        refs,
    }
}
fn node(path: &str, kind: &str, name: &str, language: &str) -> Value {
    json!({"id":format!("symbol:{path}::{name}"),"kind":kind,"name":name,"path":path,"language":language})
}
fn reference(value: &str) -> Value {
    json!({"kind":"name","value":value,"relation":"calls","line":1})
}
#[test]
fn imports_precede_global_and_heredocs_do_not_bind() {
    let items = vec![
        file(
            "src/pkg.py",
            "python",
            vec![node("src/pkg.py", "function", "run", "python")],
            vec![],
        ),
        file(
            "else.py",
            "python",
            vec![node("else.py", "function", "run", "python")],
            vec![],
        ),
        file(
            "caller.py",
            "python",
            vec![],
            vec![
                json!({"kind":"module","value":"pkg","relation":"imports","binding":"p","line":1}),
                reference("p.run"),
                reference("run"),
            ],
        ),
        file(
            "caller.sh",
            "shell",
            vec![],
            vec![
                json!({"kind":"module","value":"pkg","relation":"imports","binding":"p","heredoc_python":true,"line":1}),
                reference("p.run"),
            ],
        ),
    ];
    let (_, edges) = resolve(&items).unwrap();
    assert!(edges.iter().any(|e| e["source"] == "file:caller.py"
        && e["target"] == "symbol:src/pkg.py::run"
        && e["confidence"] == "EXTRACTED"));
    assert_eq!(
        edges
            .iter()
            .filter(|e| e["confidence"] == "AMBIGUOUS")
            .count(),
        2
    );
    assert!(
        !edges
            .iter()
            .any(|e| e["source"] == "file:caller.sh" && e["relation"] == "calls")
    );
}
#[test]
fn collisions_and_fanout() {
    let mut items = Vec::new();
    for i in 0..1000 {
        let p = format!("p{i}.py");
        items.push(file(
            &p,
            "python",
            vec![node(&p, "function", "run", "python")],
            vec![],
        ));
    }
    items.push(file("call.py", "python", vec![], vec![reference("run")]));
    let (_, edges) = resolve(&items).unwrap();
    assert_eq!(edges.len(), 1000);
    assert!(
        edges
            .iter()
            .all(|e| e["evidence"]["candidate_count"] == 1000
                && e["evidence"].get("candidates").is_none())
    );
    let first = json!({"id":"env:X","kind":"env","name":"X","language":"env","marker":1});
    let mut last = first.clone();
    last["marker"] = json!(2);
    let (nodes, _) = resolve(&[
        file("a", "shell", vec![first], vec![]),
        file("b", "shell", vec![last], vec![]),
    ])
    .unwrap();
    assert_eq!(nodes[0]["marker"], 1);
}
#[test]
fn rejects_malformed_and_accepts_empty_and_zero() {
    assert_eq!(resolve(&[]).unwrap(), (vec![], vec![]));
    let mut r = reference("x");
    r["line"] = json!(0);
    assert!(resolve(&[file("a", "python", vec![], vec![r.clone()])]).is_ok());
    r["line"] = json!("bad");
    assert!(resolve(&[file("a", "python", vec![], vec![r])]).is_err());
    assert!(resolve(&[file("a", "python", vec![json!({})], vec![])]).is_err());
}

#[test]
fn actual_legacy_extractions_match_oracle() {
    let fixture: Value = serde_json::from_str(include_str!("legacy_fixture.json")).unwrap();
    let records: Vec<Extraction> = fixture["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| Extraction {
            path: v["path"].as_str().unwrap().into(),
            sha256: v["sha256"].as_str().unwrap().into(),
            parser: v["parser"].as_str().unwrap().into(),
            nodes: v["nodes"].as_array().unwrap().clone(),
            edges: v["edges"].as_array().unwrap().clone(),
            refs: v["refs"].as_array().unwrap().clone(),
        })
        .collect();
    let (nodes, edges) = resolve(&records).unwrap();
    assert_eq!(json!(nodes), fixture["nodes"]);
    assert_eq!(json!(edges), fixture["edges"]);
}
