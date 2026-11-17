//! SQLite is a regenerable projection of graph.json. Build it in memory and
//! publish its complete image through the same no-follow atomic writer.
use super::index::ReadGraph;
use crate::{Error, Result, util::*};
use rusqlite::{Connection, DatabaseName, OpenFlags, params};
use serde_json::Value;
use std::path::Path;
pub fn write(state: &Path, graph: &Value) -> Result<()> {
    fn database(graph: &Value) -> std::result::Result<Vec<u8>, rusqlite::Error> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE nodes(id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT,name TEXT NOT NULL,language TEXT NOT NULL,qualname TEXT NOT NULL,fold_name TEXT NOT NULL,fold_qualname TEXT NOT NULL,data TEXT NOT NULL);CREATE TABLE edges(source TEXT NOT NULL,target TEXT NOT NULL,relation TEXT NOT NULL,confidence TEXT NOT NULL,data TEXT NOT NULL);CREATE INDEX nodes_path ON nodes(path);CREATE INDEX nodes_name ON nodes(name);CREATE INDEX nodes_qualname ON nodes(qualname);CREATE INDEX nodes_fold_name ON nodes(fold_name);CREATE INDEX nodes_fold_qualname ON nodes(fold_qualname);CREATE INDEX nodes_kind ON nodes(kind);CREATE INDEX edges_source ON edges(source,relation);CREATE INDEX edges_target ON edges(target,relation);")?;
        let transaction = db.transaction()?;
        transaction.execute(
            "INSERT INTO metadata VALUES('revision',?1)",
            [s(graph, "revision")],
        )?;
        transaction.execute(
            "INSERT INTO metadata VALUES('schema',?1)",
            [graph["schema"].to_string()],
        )?;
        transaction.execute("INSERT INTO metadata VALUES('projection','1')", [])?;
        {
            let mut insert = transaction
                .prepare_cached("INSERT INTO nodes VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")?;
            for node in array(graph, "nodes") {
                insert.execute(params![
                    s(node, "id"),
                    s(node, "kind"),
                    node.get("path").and_then(Value::as_str),
                    s(node, "name"),
                    s(node, "language"),
                    s(&node["metadata"], "qualname"),
                    super::unicode::casefold(s(node, "name")),
                    super::unicode::casefold(s(&node["metadata"], "qualname")),
                    node.to_string()
                ])?;
            }
        }
        {
            let mut insert =
                transaction.prepare_cached("INSERT INTO edges VALUES(?1,?2,?3,?4,?5)")?;
            for edge in array(graph, "edges") {
                insert.execute(params![
                    s(edge, "source"),
                    s(edge, "target"),
                    s(edge, "relation"),
                    s(edge, "confidence"),
                    edge.to_string()
                ])?;
            }
        }
        transaction.commit()?;
        Ok(db.serialize(DatabaseName::Main)?.to_vec())
    }
    let image = database(graph).map_err(|e| Error::new(format!("project SQLite index: {e}")))?;
    crate::util::write(&state.join("index.sqlite3"), &image)
}

/// Open a complete, immutable projection without creating journals or sidecars.
/// A concurrent atomic replacement leaves this connection on its old inode.
pub(super) fn open_verified(state: &Path, expected_revision: &str) -> Option<Connection> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let path = state.join("index.sqlite3");
    no_symlinks(&path).ok()?;
    let root = oms_safe_fs::Root::open(state).ok()?;
    let path = root.physical_root().join("index.sqlite3");
    let before = std::fs::symlink_metadata(&path).ok()?;
    if !before.is_file() || before.is_symlink() || expected_revision.is_empty() {
        return None;
    }
    let encoded: String = path
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|b| format!("%{b:02X}"))
        .collect();
    let db = Connection::open_with_flags(
        format!("file:{encoded}?immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .ok()?;
    let after = std::fs::symlink_metadata(&path).ok()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
    ) || !after.is_file()
    {
        return None;
    }
    let metadata = |key| {
        db.query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
    };
    if metadata("revision")?.as_str() != expected_revision
        || metadata("schema")? != "2"
        || metadata("projection")? != "1"
    {
        return None;
    }
    db.prepare("SELECT id,kind,path,name,language,qualname,fold_name,fold_qualname,data FROM nodes LIMIT 0").ok()?;
    db.prepare("SELECT source,target,relation,confidence,data FROM edges LIMIT 0")
        .ok()?;
    Some(db)
}
fn sql_error(error: rusqlite::Error) -> Error {
    Error::new(format!("project SQLite query: {error}"))
}
fn rows(db: &Connection, sql: &str, value: &str) -> Result<Vec<Value>> {
    let mut statement = db.prepare_cached(sql).map_err(sql_error)?;
    let records = statement
        .query_map([value], |r| r.get::<_, String>(0))
        .map_err(sql_error)?;
    records
        .map(|r| Ok(serde_json::from_str(&r.map_err(sql_error)?)?))
        .collect()
}
impl ReadGraph for Connection {
    fn lookup(&self, id: &str) -> Result<Option<Value>> {
        Ok(rows(self, "SELECT data FROM nodes WHERE id=?1", id)?.pop())
    }
    fn path_rows(&self, path: &str) -> Result<Vec<Value>> {
        rows(
            self,
            "SELECT data FROM nodes WHERE path=?1 ORDER BY id",
            path,
        )
    }
    fn root_rows(&self) -> Result<Vec<Value>> {
        rows(
            self,
            "SELECT data FROM nodes WHERE kind IN ('file','test','document','config') AND path IS NOT NULL AND path<>?1 ORDER BY id",
            "",
        )
    }
    fn symbol_rows(&self, name: &str, fold: bool) -> Result<Vec<Value>> {
        if fold {
            rows(
                self,
                "SELECT data FROM nodes WHERE (fold_name=?1 OR fold_qualname=?1) AND kind IN ('class','function','method','symbol') ORDER BY id",
                &super::unicode::casefold(name),
            )
        } else {
            rows(
                self,
                "SELECT data FROM nodes WHERE (name=?1 OR qualname=?1) AND kind IN ('class','function','method','symbol') ORDER BY id",
                name,
            )
        }
    }
    fn adjacent(&self, id: &str, direction: &str) -> Result<Vec<Value>> {
        rows(
            self,
            if direction == "out" {
                "SELECT data FROM edges WHERE source=?1 ORDER BY source,target,relation,rowid"
            } else {
                "SELECT data FROM edges WHERE target=?1 ORDER BY source,target,relation,rowid"
            },
            id,
        )
    }
    fn test(&self, id: &str) -> Result<bool> {
        self.query_row("SELECT EXISTS(SELECT 1 FROM nodes n WHERE n.id=?1 AND (n.kind='test' OR EXISTS(SELECT 1 FROM nodes t WHERE t.id='test:'||coalesce(n.path,''))))", [id], |r| r.get(0)).map_err(sql_error)
    }
}
