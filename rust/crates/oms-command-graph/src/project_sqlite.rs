//! SQLite is a regenerable projection of graph.json. Build it in memory and
//! publish its complete image through the same no-follow atomic writer.
use crate::{Error, Result, util::*};
use rusqlite::{Connection, DatabaseName, params};
use serde_json::Value;
use std::path::Path;
pub fn write(state: &Path, graph: &Value) -> Result<()> {
    fn database(graph: &Value) -> std::result::Result<Vec<u8>, rusqlite::Error> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE nodes(id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT,name TEXT NOT NULL,language TEXT NOT NULL,data TEXT NOT NULL);CREATE TABLE edges(source TEXT NOT NULL,target TEXT NOT NULL,relation TEXT NOT NULL,confidence TEXT NOT NULL,data TEXT NOT NULL);CREATE INDEX nodes_path ON nodes(path);CREATE INDEX nodes_name ON nodes(name);CREATE INDEX edges_source ON edges(source,relation);CREATE INDEX edges_target ON edges(target,relation);")?;
        let transaction = db.transaction()?;
        transaction.execute(
            "INSERT INTO metadata VALUES('revision',?1)",
            [s(graph, "revision")],
        )?;
        transaction.execute(
            "INSERT INTO metadata VALUES('schema',?1)",
            [graph["schema"].to_string()],
        )?;
        {
            let mut insert =
                transaction.prepare_cached("INSERT INTO nodes VALUES(?1,?2,?3,?4,?5,?6)")?;
            for node in array(graph, "nodes") {
                insert.execute(params![
                    s(node, "id"),
                    s(node, "kind"),
                    node.get("path").and_then(Value::as_str),
                    s(node, "name"),
                    s(node, "language"),
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
