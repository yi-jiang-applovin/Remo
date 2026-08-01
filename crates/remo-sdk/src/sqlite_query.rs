//! Generic `sqlite.query` capability: run arbitrary SQL against any
//! `.sqlite`/`.db` file in the app's own sandbox — not tied to any specific
//! ORM (GRDB, Core Data's own backing store, a hand-rolled schema). SQLite
//! itself is the one thing basically every iOS app's persistence stack
//! bottoms out on eventually, which is what makes this genuinely generic in
//! a way an app-specific "list my Widgets table" capability never could be.
//!
//! Pure Rust (`rusqlite`), no ObjC/UIKit involved — unlike
//! `remo-objc::user_defaults`/`filesystem`, this needs no Apple-specific
//! gating at all and is fully portable/testable on any OS.
//!
//! Path resolution reuses `remo_objc::filesystem::resolve` (relative paths
//! resolve against the sandbox home directory) so `sqlite.query` composes
//! naturally with `filesystem.list` for discovering database files in the
//! first place, rather than inventing a second, separate path convention.

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

/// Runs `sql` against the database at `path` (resolved via
/// `remo_objc::filesystem::resolve`) and returns either
/// `{"columns": [...], "rows": [[...], ...]}` for a statement that produces
/// rows, or `{"rows_affected": N}` for one that doesn't (INSERT/UPDATE/
/// DELETE/DDL).
///
/// No read-only restriction: a debugging tool that can already delete
/// arbitrary files (`filesystem.delete`) gains nothing from pretending SQL
/// mutations are more dangerous — the OS sandbox is the real boundary here,
/// same reasoning `filesystem.rs` already documents.
pub fn query(path: &str, sql: &str) -> Result<Value, String> {
    let resolved = remo_objc::filesystem::resolve(path);
    let conn = Connection::open_with_flags(
        &resolved,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| format!("failed to open {}: {e}", resolved.display()))?;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("failed to prepare SQL: {e}"))?;
    let column_count = stmt.column_count();

    if column_count == 0 {
        // No columns means this statement doesn't produce rows (INSERT/
        // UPDATE/DELETE/DDL) — `execute` is what actually runs it in that
        // case; a `query`/`query_map` on a rowless statement would silently
        // never step it at all.
        let rows_affected = stmt
            .execute([])
            .map_err(|e| format!("failed to execute SQL: {e}"))?;
        return Ok(json!({ "rows_affected": rows_affected }));
    }

    let column_names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();

    let rows = stmt
        .query_map([], |row| {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                values.push(value_ref_to_json(row.get_ref(i)?));
            }
            Ok(values)
        })
        .map_err(|e| format!("failed to run query: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read a row: {e}"))?;

    Ok(json!({ "columns": column_names, "rows": rows }))
}

fn value_ref_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Number(i.into()),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            use base64::Engine;
            Value::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "remo-sdk-sqlite-test-{}-{name}.sqlite",
            std::process::id()
        ))
    }

    #[test]
    fn create_insert_and_select_round_trip() {
        let path = temp_db_path("round_trip");
        let path_str = path.to_str().unwrap();

        let create = query(
            path_str,
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)",
        );
        assert_eq!(create.unwrap(), json!({ "rows_affected": 0 }));

        let insert = query(path_str, "INSERT INTO items (name) VALUES ('widget')");
        assert_eq!(insert.unwrap(), json!({ "rows_affected": 1 }));

        let select = query(path_str, "SELECT id, name FROM items").unwrap();
        assert_eq!(
            select,
            json!({ "columns": ["id", "name"], "rows": [[1, "widget"]] })
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn null_and_real_and_blob_values_convert_correctly() {
        let path = temp_db_path("types");
        let path_str = path.to_str().unwrap();

        query(path_str, "CREATE TABLE t (n TEXT, r REAL, b BLOB)").unwrap();
        query(path_str, "INSERT INTO t VALUES (NULL, 3.5, X'0102')").unwrap();

        let select = query(path_str, "SELECT n, r, b FROM t").unwrap();
        let rows = select["rows"].as_array().unwrap();
        assert_eq!(rows[0][0], Value::Null);
        assert_eq!(rows[0][1], json!(3.5));
        assert_eq!(rows[0][2], json!("AQI="));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_sql_is_a_clear_error_not_a_panic() {
        let path = temp_db_path("invalid_sql");
        let result = query(path.to_str().unwrap(), "NOT VALID SQL AT ALL");
        assert!(result.is_err());
        std::fs::remove_file(&path).ok();
    }
}
