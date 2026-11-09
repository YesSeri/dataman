use crate::model::sql_queries::quote_identifier;

use rusqlite::{params, Connection};

use crate::error::AppResult;
use crate::model::database::INTERNAL_PREFIX;

pub(crate) fn create_catalog(conn: &Connection) -> AppResult<String> {
    initialize_ownership(conn)?;
    let mut name = "__dataman_catalog".to_owned();
    let mut suffix = 0;
    while conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
        [&name],
        |row| row.get::<_, bool>(0),
    )? {
        suffix += 1;
        name = format!("__dataman_catalog_{suffix}");
    }
    conn.execute(
        &format!(
            "CREATE TEMP TABLE IF NOT EXISTS {} (
            table_name TEXT PRIMARY KEY,
            row_count INTEGER,
            col_count INTEGER,
            text_col_count INTEGER,
            int_col_count INTEGER,
            source_names TEXT,
            sheet_kind TEXT,
            stale INTEGER
        )",
            quote_identifier(&name)
        ),
        [],
    )?;
    Ok(name)
}

fn get_tables(conn: &Connection) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let table_names = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, rusqlite::Error>>()?;
    let mut visible = Vec::new();
    for name in table_names {
        if !name.starts_with("sqlite_") && !is_owned(conn, "table", &name)? {
            visible.push(name);
        }
    }
    Ok(visible)
}

pub(crate) fn populate_catalog(conn: &Connection, catalog: &str) -> AppResult<()> {
    conn.execute(&format!("DELETE FROM {}", quote_identifier(catalog)), [])?;
    let tables = get_tables(conn)?;
    for table in tables {
        let row_count: Option<i64> = None;
        let columns = visible_columns(conn, &table, false)?;
        let col_count = columns.len();
        let int_col_count = columns.iter().filter(|(_, kind)| kind == "INTEGER").count();
        let text_col_count = columns.iter().filter(|(_, kind)| kind == "TEXT").count();

        conn.execute(
            &format!(r#"REPLACE INTO {} (table_name, row_count, col_count, text_col_count, int_col_count, source_names, sheet_kind, stale) VALUES (?1, ?2, ?3, ?4, ?5, '', 'table', 0)"#, quote_identifier(catalog)),
            params![table, row_count, col_count,text_col_count, int_col_count],
        )?;
    }
    Ok(())
}

/// Explicit column metadata distinguishes user data from session-owned columns.
pub(crate) fn visible_columns(
    conn: &Connection,
    table: &str,
    for_export: bool,
) -> AppResult<Vec<(String, String)>> {
    let mut statement =
        conn.prepare(&format!("PRAGMA table_xinfo({})", quote_identifier(table)))?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut visible = Vec::new();
    for (name, kind, hidden) in columns {
        if hidden != 1
            && !is_owned(conn, &format!("column:{table}"), &name)?
            && (!for_export || !is_owned(conn, &format!("export:{table}"), &name)?)
        {
            visible.push((name, kind));
        }
    }
    Ok(visible)
}
/// Ownership is recorded in session storage; user names never imply ownership.
pub(crate) fn initialize_ownership(conn: &Connection) -> AppResult<()> {
    let attached = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='__dataman_session')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !attached {
        conn.execute_batch("ATTACH DATABASE '' AS __dataman_session;
            CREATE TABLE __dataman_session.owned_objects (scope TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(scope, name));")?;
    }
    Ok(())
}

pub(crate) fn initialize_ownership_at(conn: &Connection, path: &std::path::Path) -> AppResult<()> {
    let attached = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='__dataman_session')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if attached {
        return Err(crate::error::AppError::from(
            "The connection already has a __dataman_session schema",
        ));
    }
    let path = path.to_str().ok_or_else(|| {
        crate::error::AppError::from("The session storage path is not valid Unicode")
    })?;
    conn.execute("ATTACH DATABASE ?1 AS __dataman_session", [path])?;
    // This file is discarded with the session, never restored after a crash.
    // Retain disk journaling for transactional rollback, but avoid durable fsyncs
    // for browsing-only identity allocation. Export destinations sync separately.
    conn.execute_batch("CREATE TABLE __dataman_session.owned_objects (scope TEXT NOT NULL,name TEXT NOT NULL,PRIMARY KEY(scope,name)); PRAGMA __dataman_session.cache_size=-2048; PRAGMA __dataman_session.synchronous=OFF;")?;
    Ok(())
}

pub(crate) fn own(conn: &Connection, scope: &str, name: &str) -> AppResult<()> {
    initialize_ownership(conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO __dataman_session.owned_objects VALUES (?1, ?2)",
        params![scope, name],
    )?;
    Ok(())
}

pub(crate) fn is_owned(conn: &Connection, scope: &str, name: &str) -> AppResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM __dataman_session.owned_objects WHERE scope=?1 AND name=?2)",
        params![scope, name],
        |row| row.get(0),
    )?)
}

pub(crate) fn disown(conn: &Connection, scope: &str, name: &str) -> AppResult<()> {
    conn.execute(
        "DELETE FROM __dataman_session.owned_objects WHERE scope=?1 AND name=?2",
        params![scope, name],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::num;
    use std::{path::Path, time};

    use rusqlite::backup;

    use super::*;

    fn setup_three_table_db() -> Result<Connection, rusqlite::Error> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, c1 TEXT);", [])?;
        conn.execute(
            "CREATE TABLE t2 (id INTEGER PRIMARY KEY, c1 TEXT, c2 TEXT, c3 TEXT);",
            [],
        )?;
        conn.execute(
            "CREATE TABLE t3 (id INTEGER PRIMARY KEY, c1 TEXT, c2 TEXT, c3 TEXT, c4 INTEGER);",
            [],
        )?;

        for i in 0..5 {
            let text = format!("lorem ipsum bla bla {}", i);
            conn.execute(&(format!("INSERT INTO t1 (c1) VALUES ('{}')", text)), [])?;
        }
        for i in 3..8 {
            let text1 = format!("lorem ipsum bla bla {}", i);
            let text2 = format!("other {}", (i + 7) / 2);
            let text3 = format!("last {}", (i + 19) / 3);
            conn.execute(
                &format!(
                    "INSERT INTO t2 (c1, c2, c3) VALUES ('{}', '{}', '{}' )",
                    text1, text2, text3
                ),
                [],
            )?;
        }
        for i in 13..21 {
            let text1 = format!("lorem ipsum bla bla {}", i);
            let text2 = format!("other {}", (i + 7) / 2);
            let text3 = format!("almost last {}", (i + 19) / 3);
            let num = (i + 9) / 4;
            conn.execute(
                &format!(
                    "INSERT INTO t3 (c1, c2,c3,c4) VALUES ('{}', '{}', '{}', '{}' )",
                    text1, text2, text3, num
                ),
                [],
            )?;
        }

        create_catalog(&conn).unwrap();
        populate_catalog(&conn, "__dataman_catalog").unwrap();
        Ok(conn)
    }
    fn write_db_to_disk(conn: &Connection) {
        let directory = tempfile::tempdir().unwrap();
        let mut dst = Connection::open(directory.path().join("test.db")).unwrap();
        let backup = backup::Backup::new(conn, &mut dst).unwrap();
        backup
            .run_to_completion(10, time::Duration::from_millis(250), None)
            .unwrap();
    }
    #[test]
    fn metadata_counts_user_columns_regardless_of_prefix() {
        let connection = Connection::open_in_memory().unwrap();
        create_catalog(&connection).unwrap();
        connection.execute_batch("CREATE TABLE \"a\"\"b\" (id INTEGER, \"c\"\"d\" TEXT, __dataman_undo_hidden INTEGER); INSERT INTO \"a\"\"b\" VALUES (1, 'x', 0);").unwrap();
        populate_catalog(&connection, "__dataman_catalog").unwrap();
        let counts = connection.query_row("SELECT row_count, col_count, text_col_count, int_col_count FROM __dataman_catalog WHERE table_name = ?1", ["a\"b"], |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))).unwrap();
        assert_eq!(counts, (None, 3, 1, 2));
    }

    #[test]
    fn __dataman_catalog_num_count_test() {
        let conn = setup_three_table_db().unwrap();
        let num_tables = "SELECT COUNT(*) FROM __dataman_catalog";
        let mut stmt = conn.prepare(num_tables).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 3);
        write_db_to_disk(&conn);
    }

    #[test]
    fn __dataman_catalog_int_col_count() {
        let conn = setup_three_table_db().unwrap();
        let num_tables_fun = |table| {
            format!("SELECT int_col_count FROM __dataman_catalog WHERE table_name = '{table}'")
        };
        let mut stmt = conn.prepare(&num_tables_fun("t2")).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        let mut stmt = conn.prepare(&num_tables_fun("t3")).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
        write_db_to_disk(&conn);
    }

    #[test]
    fn __dataman_catalog_text_col_count() {
        let conn = setup_three_table_db().unwrap();
        let num_tables_fun = |table| {
            format!("SELECT text_col_count FROM __dataman_catalog WHERE table_name = '{table}'")
        };

        let mut stmt = conn.prepare(&num_tables_fun("t1")).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        let mut stmt = conn.prepare(&num_tables_fun("t2")).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 3);
        let mut stmt = conn.prepare(&num_tables_fun("t3")).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 3);
        write_db_to_disk(&conn);
    }
}
