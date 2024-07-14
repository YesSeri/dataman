use std::fmt;

use ratatui::text;

use rusqlite::{params, Connection};

use crate::error::AppResult;

fn create_table_of_tables(conn: &Connection) -> AppResult<()> {
    conn.execute("DROP TABLE IF EXISTS table_of_tables;", [])?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS table_of_tables (
            table_name TEXT PRIMARY KEY,
            row_count INTEGER,
            col_count INTEGER,
            text_col_count INTEGER,
            int_col_count INTEGER
        )",
        [],
    )?;
    Ok(())
}

fn get_tables(conn: &Connection) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'table_of_tables'")?;
    let table_names = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, rusqlite::Error>>()?;
    Ok(table_names)
}

fn get_table_row_count(conn: &Connection, table_name: &str) -> AppResult<i64> {
    let mut stmt = conn.prepare(&format!(r#"SELECT COUNT(*) FROM "{}""#, table_name))?;
    let row_count: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(row_count)
}

fn get_table_col_count(conn: &Connection, table_name: &str) -> AppResult<usize> {
    let mut stmt = conn.prepare(&format!(r#"PRAGMA table_info("{}")"#, table_name))?;
    let col_count = stmt.query_map([], |_row| Ok(()))?.count();
    Ok(col_count)
}

#[derive(Debug, PartialEq)]
enum ColumnKind {
    Text,
    Int,
}

impl fmt::Display for ColumnKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ColumnKind::Text => write!(f, "TEXT"),
            ColumnKind::Int => write!(f, "INT"),
        }
    }
}

fn get_table_kind_col_count(
    conn: &Connection,
    table_name: &str,
    kind: ColumnKind,
) -> AppResult<usize> {
    let mut stmt = conn.prepare(&format!(r#"PRAGMA table_info("{}")"#, table_name))?;
    let col_count = stmt
        .query_map([], |row| {
            let type_name: String = row.get(2)?;
            Ok(type_name)
        })?
        .filter(|x| x.as_ref() == Ok(&kind.to_string()))
        .count();
    Ok(col_count)
}
fn populate_table_of_tables(conn: &Connection) -> AppResult<()> {
    let tables = get_tables(conn)?;
    for table in tables {
        let row_count = get_table_row_count(conn, &table)?;
        let col_count = get_table_col_count(conn, &table)?;
        let int_col_count = get_table_kind_col_count(conn, &table, ColumnKind::Int)?;
        let text_col_count = get_table_kind_col_count(conn, &table, ColumnKind::Text)?;

        conn.execute(
            r#"INSERT INTO table_of_tables (table_name, row_count, col_count, text_col_count, int_col_count) VALUES (?1, ?2, ?3, ?4, ?5)"#,
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
            "CREATE TABLE t3 (id INTEGER PRIMARY KEY, c1 TEXT, c2 TEXT, c3 TEXT, c4 INT);",
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
        let dst = Path::new("test.db");
        let mut dst = Connection::open(dst).unwrap();
        let backup = backup::Backup::new(conn, &mut dst).unwrap();
        backup
            .run_to_completion(10, time::Duration::from_millis(250), None)
            .unwrap();
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
        assert_eq!(count, 0);
        let mut stmt = conn.prepare(&num_tables_fun("t3")).unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
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
