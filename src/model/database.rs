use std::path::{Path, PathBuf};
use std::time;

use rusqlite::{backup, params, Connection, Statement};

use crate::error::{AppError, AppResult};
use crate::model::datarow::DataItem;

use super::datarow::DataTable;
use super::metadata::{create_table_of_tables, populate_table_of_tables};
use super::regexping;
use super::sql_queries;

#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
}

impl Database {
    pub fn new(connection: Connection) -> AppResult<Self> {
        create_table_of_tables(&connection)?;
        if let Err(err) = regexping::custom_functions::add_custom_functions(&connection) {
            log::info!("Error adding custom functions, e.g. REGEXP: {}", err);
            Err(AppError::Sqlite(err))
        } else {
            Ok(Database { connection })
        }
    }
    pub(crate) fn backup_db<P: AsRef<Path>>(&self, dst: P) -> AppResult<()> {
        let mut stmt = self.connection.prepare("PRAGMA page_count")?;
        let page_count: i32 = stmt.query_row([], |row| row.get(0)).unwrap_or(i32::MAX);

        let mut dst = Connection::open(dst)?;
        let backup = backup::Backup::new(&self.connection, &mut dst)?;
        backup
            .run_to_completion(page_count, time::Duration::from_millis(250), None)
            .map_err(AppError::from)
    }

    pub fn get(
        &self,
        limit: u32,
        offset: u32,
        table_name: &str,
        ordering: &str,
    ) -> AppResult<DataTable> {
        let query = format!(
            r#"SELECT * FROM "{}" {} LIMIT {} OFFSET {};"#,
            table_name, ordering, limit, offset
        );

        let (headers, data_rows) = {
            let mut data_rows = vec![];
            let mut stmt = self.prepare(&query).unwrap();
            let headers: Vec<String> = stmt
                .column_names()
                .into_iter()
                .map(|h| h.to_string())
                .collect();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let mut items = vec![];
                let mut i = 0;
                while let Ok(field) = row.get_ref(i) {
                    items.push(DataItem::from(field));
                    i += 1;
                }
                data_rows.push(items);
            }
            (headers, data_rows)
        };
        Ok((headers, data_rows))
    }
    pub(crate) fn count_rows(&self, table_name: &str) -> Option<u32> {
        self.connection
            .query_row(
                &format!(r#"SELECT COUNT(*) FROM "{table_name}";"#),
                [],
                |row| row.get(0),
            )
            .ok()
    }
    pub fn get_cell(&self, table_name: &str, id: i32, header: &str) -> AppResult<String> {
        let query = format!(r#"SELECT "{}" FROM "{}" WHERE id = ?;"#, header, table_name);
        let mut stmt = self.prepare(&query)?;
        log::info!("id: {}", id);
        let mut rows = stmt.query(params![id])?;
        let row = rows.next()?.unwrap();
        let cell = row.get(0)?;

        Ok(cell)
    }
    fn prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>> {
        log::info!("{sql}");
        self.connection.prepare(sql)
    }
    fn execute<P: rusqlite::Params>(&self, sql: &str, params: P) -> AppResult<()> {
        log::info!("{sql}");
        self.connection.execute(sql, params)?;
        Ok(())
    }

    pub fn execute_batch(&self, sql: &str) -> AppResult<()> {
        let query = &format!(
            r#"BEGIN TRANSACTION;
				{}
			COMMIT;"#,
            sql
        );
        if cfg!(debug_assertions) {
            log::info!("{query}");
        }

        match self.connection.execute_batch(query) {
            Ok(_) => Ok(()),
            Err(err) => {
                self.execute("ROLLBACK;", [])?;
                log::info!("Error executing batch query: {}", err);
                Err(AppError::Sqlite(err))
            }
        }
    }
    pub fn derive_column<F>(
        &self,
        table_name: &str,
        old_column_name: &str,
        new_column_name: &str,
        fun: F,
    ) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        // create a new column in the table. The new value for each row is the value string value of column name after running fun function on it.
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let query = format!(r#"SELECT "id", "{old_column_name}" FROM "{table_name}""#);
        let mut binding = self.prepare(&query)?;
        let mut rows = binding.query([])?;
        let create_column_query =
            format!(r#"ALTER TABLE "{table_name}" ADD COLUMN '{new_column_name}' TEXT;"#);
        dbg!(&create_column_query);
        // return Ok(());
        let mut transaction = String::new();
        transaction.push_str(create_column_query.as_ref());
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value: String = row.get(1)?;
            let derived_value = fun(value).unwrap_or("NULL".to_string()).replace('\'', "''");
            let update_query = format!(
                r#"UPDATE "{table_name}" SET "{new_column_name}" = '{derived_value}' WHERE id = '{id}';"#,
            );
            transaction.push_str(&update_query);
        }
        self.execute_batch(&transaction)?;
        Ok(())
    }

    pub fn get_table_names(&self) -> AppResult<Vec<String>> {
        let query = r#"SELECT name FROM sqlite_master WHERE type='table' ORDER BY rowid;"#;
        let mut stmt = self.prepare(query)?;
        let mut rows = stmt.query([])?;
        let mut table_names = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            table_names.push(name);
        }
        Ok(table_names)
    }
    fn find_unused_table_name(&self, table_name: &str) -> AppResult<String> {
        let table_names = self.get_table_names()?;
        let mut new_table_name = table_name.to_string();
        let mut suffix = 1;
        while table_names.contains(&new_table_name) {
            new_table_name = format!("{}_{}", table_name, suffix);
            suffix += 1;
        }
        Ok(new_table_name)
    }

    fn find_unused_header_name(&self, table_name: &str, header_name: &str) -> AppResult<String> {
        let header_names = self.get_headers(&table_name)?;
        let mut new_header_name = header_name.to_string();
        let mut suffix = 1;
        while header_names.contains(&new_header_name) {
            new_header_name = format!("{}_{}", header_name, suffix);
            suffix += 1;
        }
        // log everything
        Ok(new_header_name)
    }
    pub fn regex_filter(&self, table_name: &str, header: &str, pattern: &str) -> AppResult<String> {
        // create new table with filter applied using create table as sqlite statement.
        let old_table_name = table_name;
        let temp_table_name = format!("{}_filt", old_table_name);
        let new_table_name = self.find_unused_table_name(&temp_table_name)?;
        let (query, new_table_name) =
            regexping::regex_filter_query(header, pattern, &old_table_name, &new_table_name)?;

        let res = self.execute(&query, []);
        res?;
        Ok(new_table_name)
    }

    // go to first match
    pub(crate) fn exact_search(
        &self,
        table_name: &str,
        ordering: &str,
        current_row: u32,
        search_header: &str,
        pattern: &str,
    ) -> AppResult<u32> {
        let query = sql_queries::build::exact_search_query(
            ordering,
            search_header,
            current_row,
            &table_name,
        );

        let row_number: u32 = self
            .connection
            .query_row(&query, [pattern], |row| row.get(0))?;
        Ok(row_number - 1)
    }

    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.
    pub(crate) fn regex_capture_group_transform(
        &self,
        table_name: &str,
        pattern: &str,
        header: &str,
        transformation: &str,
    ) -> AppResult<()> {
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let transform_header = format!("{}_tform", header);
        let new_header_name = self.find_unused_header_name(table_name, &transform_header)?;
        log::error!("{} {} {}", table_name, transform_header, new_header_name);
        let queries = regexping::regex_with_capture_group_transform_query(
            header,
            &new_header_name,
            pattern,
            transformation,
            &table_name,
        )?;
        self.execute_batch(&queries)?;
        Ok(())
    }
    pub(crate) fn regex_no_capture_group_transform(
        &self,
        table_name: &str,
        pattern: &str,
        header: &str,
    ) -> AppResult<()> {
        let transform_header = format!("{}_tform", header);
        let new_header_name = self.find_unused_header_name(table_name, &transform_header)?;
        let queries = regexping::regex_no_capture_group_transform_query(
            header,
            &new_header_name,
            pattern,
            &table_name,
        )?;
        self.execute_batch(&queries)?;
        Ok(())
    }

    pub(crate) fn copy(&self, table_name: &str, header: &str) -> AppResult<()> {
        let transform_header = format!("{}_copy", header);
        let new_header_name = self.find_unused_header_name(table_name, &transform_header)?;
        // let derived_header_name = format!("derived_{}", header);
        let create_header_query =
            format!(r#"ALTER TABLE "{table_name}" ADD COLUMN "{new_header_name}" TEXT;"#);

        let mut queries = String::new();
        queries.push_str(&create_header_query);
        let update_query =
            format!(r#"UPDATE "{table_name}" SET "{new_header_name}" = "{header}";"#);
        queries.push_str(&update_query);
        self.execute_batch(&queries)
    }
    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.

    pub(crate) fn sql_query(&self, query: &str) -> AppResult<()> {
        self.execute_batch(query)
    }

    pub(crate) fn get_table_name(file: PathBuf) -> Option<String> {
        file.file_stem().map(|el| el.to_string_lossy().into_owned())
    }

    pub fn get_headers(&self, table_name: &str) -> AppResult<Vec<String>> {
        let query = format!(r#"PRAGMA table_info("{}")"#, table_name);
        let mut stmt = self.connection.prepare(&query)?;
        let column_names: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .map(|result| result.expect("Failed to retrieve column name"))
            .collect();
        Ok(column_names)
    }
    pub fn count_headers(&self, table_name: &str) -> AppResult<u16> {
        Ok(self.get_headers(&table_name)?.len() as u16)
    }

    pub fn update_cell(
        &self,
        table_name: &str,
        header: &str,
        id: i32,
        content: &str,
    ) -> AppResult<()> {
        let update_query = format!(r#"UPDATE "{table_name}" SET '{header}' = ? WHERE id = ?;"#);
        self.execute(&update_query, params![content, id])?;
        Ok(())
    }

    pub(crate) fn text_to_int(&self, table_name: &str, column: &str) -> AppResult<()> {
        let queries = sql_queries::build::text_to_int(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn int_to_text(&self, table_name: &str, column: &str) -> AppResult<()> {
        let queries = sql_queries::build::int_to_text_query(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_column(&self, table_name: &str, column: &str) -> AppResult<String> {
        let queries = sql_queries::build::delete_column_query(&table_name, &column);
        self.execute_batch(&queries)?;
        Ok(format!("Deleted column {column} from {table_name}"))
    }

    pub(crate) fn rename_column(
        &self,
        table_name: &str,
        column: &str,
        new_column: &str,
    ) -> AppResult<()> {
        let queries = sql_queries::build::rename_column_query(&table_name, &column, new_column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_table(&self, table_name: &str) -> AppResult<()> {
        let query = sql_queries::build::delete_table_query(&table_name);
        self.execute(&query, [])?;
        log::info!("Deleted table {table_name}");
        Ok(())
    }

    pub(crate) fn rename_table(&self, old_table_name: &str, new_table_name: &str) -> AppResult<()> {
        let query = sql_queries::build::rename_table_query(old_table_name, new_table_name);
        self.execute(&query, [])?;
        Ok(())
    }

    // TODO if the column contains a float, 3.0, then ensure that ALL intermediary calculations are done with floats.
    // currently (3/2)*2.0 = 2.0, but it should be 3.0.
    pub(crate) fn math_operation(&self, table_name: &str, inputs: Vec<String>) -> AppResult<()> {
        let math_expr = inputs[0].clone();
        let new_math_expr_col = self.find_unused_header_name(table_name, "math_expr")?;
        let query =
            sql_queries::build::math_expression_query(&new_math_expr_col, table_name, &math_expr);
        self.execute_batch(&query)
    }

    pub(crate) fn view_metadata_table(&self) -> Result<(), AppError> {
        populate_table_of_tables(&self.connection)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn setup_database() -> Database {
        Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap()
    }
    fn setup_three_table_db() -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        connection
            .execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        connection
            .execute("CREATE TABLE t3 (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        Database::new(connection).unwrap()
    }

    #[test]
    fn get_number_of_headers_test() {
        let database = setup_database();
        let number_of_headers = database.count_headers("data").unwrap();
        assert_eq!(number_of_headers, 4)
    }

    #[test]
    fn derive_column_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let col = "firstname";
        let col_new = "firstname_derived";
        let fun = |s| Some(format!("{}-changed", s));
        database.derive_column("data", col, col_new, fun).unwrap();
        let first: String = database.get(1, 0, "data", "").unwrap().1[0]
            .get(4)
            .unwrap()
            .to_string();
        assert_eq!(first, "henrik-changed");
        let n = database.count_headers("data").unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn update_cell() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        database
            .update_cell("data", "firstname", 1, "hank")
            .unwrap();
        let first: String = database.get(1, 0, "data", "").unwrap().1[0]
            .get(1)
            .unwrap()
            .to_string();

        let (_, rows) = database.get(1, 0, "data", "").unwrap();

        // assert_eq!(first, "hank");
    }

    #[test]
    fn test_offset() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();

        // we use limit 2 so we can check first and second item
        let (headers, data_rows) = database.get(2, 0, "data", "").unwrap();
        let first: String = data_rows[0].get(1).unwrap().to_string();
        assert_eq!("henrik", first);

        // we can look into the old data rows to see the next item:
        let second: String = data_rows[1].get(1).unwrap().to_string();
        assert_eq!("john", second);

        // or we can update the 'cache'
        // this makes us get a new view into the database
        // if we don't use this we would need to index into [1] instead
        let (headers, data_rows) = database.get(1, 1, "data", "").unwrap();
        let second: String = data_rows[0].get(1).unwrap().to_string();
        assert_eq!("john", second);
    }

    #[test]
    fn get_table_names_test() {
        let database = setup_database();
        let table_names = database.get_table_names().unwrap();
    }

    #[test]
    fn get_headers_test() {
        let database = setup_database();
        let headers = database.get_headers("data").unwrap();
        assert_eq!(headers, vec!["id", "firstname", "lastname", "age"]);
    }

    #[test]
    fn count_rows_test() {
        let database = setup_database();
        let rows_len = database.count_rows("data").unwrap();
        assert_eq!(rows_len, 6)
    }

    #[test]
    fn custom_functions_regexp_test() {
        let database = setup_database();
        let query = r#"SELECT firstname FROM "data" WHERE regexp('h.*k', firstname)"#;
        let result: String = database
            .connection
            .query_row(query, [], |row| row.get(0))
            .unwrap();
        assert_eq!(result, "henrik");
    }
    #[test]
    fn table_of_tables_test() {
        let database = setup_three_table_db();
    }

    #[test]
    fn copy_column_long_test() {
        let p = vec![PathBuf::from("assets/data-long.csv")];
        let database = Database::try_from(p).unwrap();

        let copy_fun = |s: String| Some(s.to_string());
        let column_name = "firstname";
        let new_column_name = database
            .find_unused_header_name("data", column_name)
            .unwrap();
        database
            .derive_column("data", column_name, &new_column_name, copy_fun)
            .unwrap();
        let (_, res) = database.get(20, 0, "data", "").unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_test() {
        let p = vec![PathBuf::from("assets/data.csv")];
        let database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| Some(s.to_string());

        let column_name = "firstname";
        let new_column_name = database
            .find_unused_header_name("data", column_name)
            .unwrap();
        database
            .derive_column("data", column_name, &new_column_name, copy_fun)
            .unwrap();
        let (_, res) = database.get(20, 100, "data", "").unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }
}
