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
