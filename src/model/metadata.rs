use crate::model::sql_queries::quote_identifier;

use rusqlite::{params, Connection};

use crate::error::AppResult;
use crate::model::database::INTERNAL_PREFIX;

pub(crate) fn create_table_of_tables(conn: &Connection) -> AppResult<()> {
    conn.execute("DROP TABLE IF EXISTS table_of_tables;", [])?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS table_of_tables (
            table_name TEXT PRIMARY KEY,
            row_count INTEGER,
            col_count INTEGER,
            text_col_count INTEGER,
            int_col_count INTEGER,
            source_names TEXT,
            sheet_kind TEXT,
            stale INTEGER
        )",
        [],
    )?;
    Ok(())
}

fn get_tables(conn: &Connection) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let table_names = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, rusqlite::Error>>()?;
    Ok(table_names
        .into_iter()
        .filter(|name: &String| {
            !name.starts_with("sqlite_")
                && !name.starts_with(INTERNAL_PREFIX)
                && name != "table_of_tables"
        })
        .collect())
}

pub(crate) fn populate_table_of_tables(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM table_of_tables", [])?;
    let tables = get_tables(conn)?;
    for table in tables {
        let row_count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM {}", quote_identifier(&table)),
            [],
            |row| row.get(0),
        )?;
        let mut statement =
            conn.prepare(&format!("PRAGMA table_info({})", quote_identifier(&table)))?;
        let columns = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let columns = columns
            .iter()
            .filter(|(name, _)| !name.starts_with(INTERNAL_PREFIX))
            .collect::<Vec<_>>();
        let col_count = columns.len();
        let int_col_count = columns.iter().filter(|(_, kind)| kind == "INTEGER").count();
        let text_col_count = columns.iter().filter(|(_, kind)| kind == "TEXT").count();

        conn.execute(
            r#"REPLACE INTO table_of_tables (table_name, row_count, col_count, text_col_count, int_col_count, source_names, sheet_kind, stale) VALUES (?1, ?2, ?3, ?4, ?5, '', 'table', 0)"#,
            params![table, row_count, col_count,text_col_count, int_col_count],
        )?;
    }
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

        create_table_of_tables(&conn).unwrap();
        populate_table_of_tables(&conn).unwrap();
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
    fn metadata_counts_quoted_identifiers_and_excludes_internal_columns() {
        let connection = Connection::open_in_memory().unwrap();
        create_table_of_tables(&connection).unwrap();
        connection.execute_batch("CREATE TABLE \"a\"\"b\" (id INTEGER, \"c\"\"d\" TEXT, __dataman_undo_hidden INTEGER); INSERT INTO \"a\"\"b\" VALUES (1, 'x', 0);").unwrap();
        populate_table_of_tables(&connection).unwrap();
        let counts = connection.query_row("SELECT row_count, col_count, text_col_count, int_col_count FROM table_of_tables WHERE table_name = ?1", ["a\"b"], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))).unwrap();
        assert_eq!(counts, (1, 2, 1, 1));
    }

    #[test]
    fn table_of_tables_num_count_test() {
        let conn = setup_three_table_db().unwrap();
        let num_tables = "SELECT COUNT(*) FROM table_of_tables";
        let mut stmt = conn.prepare(num_tables).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 3);
        write_db_to_disk(&conn);
    }

    #[test]
    fn table_of_tables_int_col_count() {
        let conn = setup_three_table_db().unwrap();
        let num_tables_fun = |table| {
            format!("SELECT int_col_count FROM table_of_tables WHERE table_name = '{table}'")
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
    fn table_of_tables_text_col_count() {
        let conn = setup_three_table_db().unwrap();
        let num_tables_fun = |table| {
            format!("SELECT text_col_count FROM table_of_tables WHERE table_name = '{table}'")
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
