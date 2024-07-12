use crate::error::AppResult;

pub fn regex_filter_query(header: &str, pattern: &str, table_name: &str) -> AppResult<String> {
    // create new table with filter applied using create table as sqlite statement.
    regex::Regex::new(pattern)?;
    let select_query =
        format!(r#"SELECT * FROM "{table_name}" WHERE "{header}" REGEXP '{pattern}'"#);
    let create_table_query =
        format!(r#"CREATE TABLE "{table_name}RegexFiltered" AS {select_query};"#);
    Ok(create_table_query)
}

pub(crate) fn regex_with_capture_group_transform_query(
    header: &str,
    pattern: &str,
    transformation: &str,
    table_name: &str,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    let derived_header_name = format!("derived{}", header);
    let create_header_query =
        format!(r#"ALTER TABLE "{table_name}" ADD COLUMN "{derived_header_name}" TEXT;"#);

    let mut queries = String::new();
    queries.push_str(&create_header_query);
    let update_query = format!(
        r#"UPDATE "{table_name}" SET "{derived_header_name}" = regexp_transform_with_capture_group('{pattern}', "{header}", '{transformation}');"#
    );

    queries.push_str(&update_query);
    Ok(queries)
}

pub(crate) fn regex_no_capture_group_transform_query(
    header: &str,
    pattern: &str,
    table_name: &str,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    // for each row in the table, run fun on the value of column name and insert the result into the new column
    let derived_header_name = format!("derived{}", header);
    let create_header_query =
        format!(r#"ALTER TABLE "{table_name}" ADD COLUMN "{derived_header_name}" TEXT;"#);

    let mut queries = String::new();
    queries.push_str(&create_header_query);
    let update_query = format!(
        r#"UPDATE "{table_name}" SET "{derived_header_name}" = regexp_transform_no_capture_group('{pattern}', "{header}");"#
    );

    queries.push_str(&update_query);
    Ok(queries)
}

pub mod custom_functions {
    use core::hash;
    use std::{
        collections::{hash_map, HashMap},
        sync::{Arc, Mutex, RwLock},
    };

    use regex::Regex;
    use rusqlite::{functions::FunctionFlags, types::ValueRef, Connection};

    use crate::model::database::Database;

    pub fn add_custom_functions(conn: &Connection) -> rusqlite::Result<()> {
        let hash_map: HashMap<String, Regex> = HashMap::new();
        let regex_cache: Arc<Mutex<HashMap<String, Regex>>> = Arc::new(Mutex::new(HashMap::new()));

        let cached_filter_regex = Arc::new(Mutex::new(Regex::new("").unwrap()));
        conn.create_scalar_function(
            "regexp",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1)?;
                let mut cached_filter_regex = cached_filter_regex.lock().unwrap();
                if cached_filter_regex.as_str() != regex_str {
                    *cached_filter_regex = Regex::new(&regex_str).unwrap();
                }

                Ok(cached_filter_regex.is_match(&text))
            },
        )?;
        let cached_with_capture_regex = Arc::new(Mutex::new(Regex::new("").unwrap()));
        conn.create_scalar_function(
            // this one is used to filter, to create new tables
            "regexp_transform_with_capture_group",
            3,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1)?;
                let substitution_pattern = ctx.get::<String>(2)?;
                let mut cached_with_capture_regex = cached_with_capture_regex.lock().unwrap();
                if cached_with_capture_regex.as_str() != regex_str {
                    *cached_with_capture_regex = Regex::new(&regex_str).unwrap();
                }
                let is_match = cached_with_capture_regex.is_match(&text);
                if is_match {
                    let val = cached_with_capture_regex
                        .replace(&text, &substitution_pattern)
                        .to_string();
                    Ok(Some(val))
                } else {
                    Ok(None)
                }
            },
        )?;
        let cached_no_capture_regex = Arc::new(Mutex::new(Regex::new("").unwrap()));
        conn.create_scalar_function(
            // this is used to derive a new column
            "regexp_transform_no_capture_group",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1)?;
                let mut cached_no_capture_regex = cached_no_capture_regex.lock().unwrap();
                if cached_no_capture_regex.as_str() != regex_str {
                    *cached_no_capture_regex = Regex::new(&regex_str).unwrap();
                }
                let result = cached_no_capture_regex.captures(&text);
                let val = result
                    .and_then(|c| c.get(0))
                    .map(|v| v.as_str().to_string());
                Ok(val)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::database::Database;
    use crate::model::db_slice::DatabaseSlice;
    use custom_functions::add_custom_functions;
    use ratatui::widgets::TableState;
    use rusqlite::{Connection, Result};
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    fn setup_empty_database() -> Result<Database> {
        let connection = Connection::open_in_memory()?;
        let database = Database::new(connection).unwrap();
        // let paths = vec![PathBuf::from("./assets/customers-1000000.csv")];
        // let database = Database::try_from(paths).unwrap();
        Ok(database)
    }

    #[test]
    fn test_regexp_function_single_threaded() -> Result<()> {
        let database = setup_empty_database()?;
        let conn = &database.connection;

        let mut stmt = conn.prepare("SELECT regexp(?, ?)")?;
        let regex_str = r"\d+";
        let text = "123abc";

        let start = Instant::now();
        let result: bool = stmt.query_row((regex_str, text), |row| row.get(0))?;
        let duration = start.elapsed();

        assert!(result);
        println!("Single-threaded test duration: {:?}", duration);

        Ok(())
    }

    fn setup_large_database(num_rows: usize) -> Result<Database> {
        let conn = Connection::open_in_memory()?;
        {
            conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, text TEXT)", [])?;
            let mut stmt = conn.prepare("INSERT INTO test (text) VALUES (?)")?;

            for i in 0..num_rows {
                let text = format!("Sample text with number {}", i);
                stmt.execute([text])?;
            }
        }
        let database = Database::new(conn).unwrap();
        Ok(database)
    }

    #[test]
    fn test_regexp_function_small_dataset() -> Result<()> {
        let num_rows = 10000;
        let database = setup_large_database(num_rows)?;
        let conn = &database.connection;

        let mut stmt = conn.prepare("SELECT regexp(?, text) FROM test")?;
        let regex_str = r"\d+";

        let start = Instant::now();
        let mut count = 0;
        let mut rows = stmt.query([regex_str])?;
        while let Some(row) = rows.next()? {
            let result: bool = row.get(0)?;
            if result {
                count += 1;
            }
        }
        let duration = start.elapsed();

        println!("Large dataset test duration: {:?}", duration);
        println!("Number of matches: {}", count);

        Ok(())
    }

    #[test]
    fn test_regexp_function_large_dataset() -> Result<()> {
        let num_rows = 1000000;
        let database = setup_large_database(num_rows)?;
        let conn = &database.connection;

        let mut stmt = conn.prepare("SELECT regexp(?, text) FROM test")?;
        let regex_str = r"\d+";

        let start = Instant::now();
        let mut count = 0;
        let mut rows = stmt.query([regex_str])?;
        while let Some(row) = rows.next()? {
            let result: bool = row.get(0)?;
            if result {
                count += 1;
            }
        }
        let duration = start.elapsed();

        println!("Large dataset test duration: {:?}", duration);
        println!("Number of matches: {}", count);

        Ok(())
    }

    #[test]
    fn test_regexp_transform_with_capture_group_large_dataset() -> Result<()> {
        let num_rows = 1000000;
        let database = setup_large_database(num_rows)?;
        let conn = &database.connection;

        let mut stmt =
            conn.prepare("SELECT regexp_transform_with_capture_group(?, text, ?) FROM test")?;
        let regex_str = r"(\d+)";
        let substitution_pattern = r"$1 transformed";

        let start = Instant::now();
        let mut count = 0;
        let mut rows = stmt.query((regex_str, substitution_pattern))?;
        while let Some(row) = rows.next()? {
            let result: Option<String> = row.get(0)?;
            if let Some(val) = result {
                count += 1;
                // Optionally print some transformed values
                if count <= 5 {
                    println!("Transformed value: {}", val);
                }
            }
        }
        let duration = start.elapsed();

        println!(
            "Large dataset transform with capture group test duration: {:?}",
            duration
        );
        println!("Number of transformations: {}", count);

        Ok(())
    }

    #[test]
    fn test_regexp_transform_no_capture_group_large_dataset() -> Result<()> {
        let num_rows = 1000000;
        let database = setup_large_database(num_rows)?;
        let conn = &database.connection;

        let mut stmt =
            conn.prepare("SELECT regexp_transform_no_capture_group(?, text) FROM test")?;
        let regex_str = r"\d+";

        let start = Instant::now();
        let mut count = 0;
        let mut rows = stmt.query([regex_str])?;
        while let Some(row) = rows.next()? {
            let result: Option<String> = row.get(0)?;
            if let Some(val) = result {
                count += 1;
                // Optionally print some transformed values
                if count <= 5 {
                    println!("Transformed value: {}", val);
                }
            }
        }
        let duration = start.elapsed();

        println!(
            "Large dataset transform no capture group test duration: {:?}",
            duration
        );
        println!("Number of transformations: {}", count);

        Ok(())
    }
    // #[test]
    // fn test_regexp_function_multi_threaded() -> Result<()> {
    //     let database = Arc::new(setup_database()?);
    //     let regex_str = Arc::new(r"\d+".to_string());
    //     let text = Arc::new("123abc".to_string());
    //     let barrier = Arc::new(Barrier::new(10));
    //     let duration_mutex = Arc::new(Mutex::new(Duration::new(0, 0)));

    //     let mut handles = vec![];
    //     for _ in 0..10 {
    //         let database = Arc::clone(&database);
    //         let regex_str = Arc::clone(&regex_str);
    //         let text = Arc::clone(&text);
    //         let barrier = Arc::clone(&barrier);
    //         let duration_mutex = Arc::clone(&duration_mutex);

    //         let handle = thread::spawn(move || {
    //             let conn = &database.connection;
    //             let mut stmt = conn.prepare("SELECT regexp(?, ?)").unwrap();
    //             barrier.wait();

    //             let start = Instant::now();
    //             let result: bool = stmt
    //                 .query_row((&*regex_str, &*text), |row| row.get(0))
    //                 .unwrap();
    //             let duration = start.elapsed();

    //             assert!(result);
    //             let mut duration_lock = duration_mutex.lock().unwrap();
    //             *duration_lock += duration;
    //         });

    //         handles.push(handle);
    //     }

    //     for handle in handles {
    //         handle.join().unwrap();
    //     }

    //     let total_duration = *duration_mutex.lock().unwrap();
    //     println!("Multi-threaded test total duration: {:?}", total_duration);

    //     Ok(())
    // }

    // #[test]
    // fn test_regexp_transform_with_capture_group_multi_threaded() -> Result<()> {
    //     let database = Arc::new(setup_database()?);
    //     let regex_str = Arc::new(r"(\d+)".to_string());
    //     let text = Arc::new("123abc".to_string());
    //     let substitution_pattern = Arc::new("$1def".to_string());
    //     let barrier = Arc::new(Barrier::new(10));
    //     let duration_mutex = Arc::new(Mutex::new(Duration::new(0, 0)));

    //     let mut handles = vec![];
    //     for _ in 0..10 {
    //         let database = Arc::clone(&database);
    //         let regex_str = Arc::clone(&regex_str);
    //         let text = Arc::clone(&text);
    //         let substitution_pattern = Arc::clone(&substitution_pattern);
    //         let barrier = Arc::clone(&barrier);
    //         let duration_mutex = Arc::clone(&duration_mutex);

    //         let handle = thread::spawn(move || {
    //             let conn = &database.connection;
    //             let mut stmt = conn
    //                 .prepare("SELECT regexp_transform_with_capture_group(?, ?, ?)")
    //                 .unwrap();
    //             barrier.wait();

    //             let start = Instant::now();
    //             let result: Option<String> = stmt
    //                 .query_row((&*regex_str, &*text, &*substitution_pattern), |row| {
    //                     row.get(0)
    //                 })
    //                 .unwrap();
    //             let duration = start.elapsed();

    //             assert_eq!(result, Some("123def".to_string()));
    //             let mut duration_lock = duration_mutex.lock().unwrap();
    //             *duration_lock += duration;
    //         });

    //         handles.push(handle);
    //     }

    //     for handle in handles {
    //         handle.join().unwrap();
    //     }

    //     let total_duration = *duration_mutex.lock().unwrap();
    //     println!(
    //         "Multi-threaded test total duration for transform with capture group: {:?}",
    //         total_duration
    //     );

    //     Ok(())
    // }

    // #[test]
    // fn test_regexp_transform_no_capture_group_multi_threaded() -> Result<()> {
    //     let database = Arc::new(setup_database()?);
    //     let regex_str = Arc::new(r"\d+".to_string());
    //     let text = Arc::new("123abc".to_string());
    //     let barrier = Arc::new(Barrier::new(10));
    //     let duration_mutex = Arc::new(Mutex::new(Duration::new(0, 0)));

    //     let mut handles = vec![];
    //     for _ in 0..10 {
    //         let database = Arc::clone(&database);
    //         let regex_str = Arc::clone(&regex_str);
    //         let text = Arc::clone(&text);
    //         let barrier = Arc::clone(&barrier);
    //         let duration_mutex = Arc::clone(&duration_mutex);

    //         let handle = thread::spawn(move || {
    //             let conn = &database.connection;
    //             let mut stmt = conn
    //                 .prepare("SELECT regexp_transform_no_capture_group(?, ?)")
    //                 .unwrap();
    //             barrier.wait();

    //             let start = Instant::now();
    //             let result: Option<String> = stmt
    //                 .query_row((&*regex_str, &*text), |row| row.get(0))
    //                 .unwrap();
    //             let duration = start.elapsed();

    //             assert_eq!(result, Some("123".to_string()));
    //             let mut duration_lock = duration_mutex.lock().unwrap();
    //             *duration_lock += duration;
    //         });

    //         handles.push(handle);
    //     }

    //     for handle in handles {
    //         handle.join().unwrap();
    //     }

    //     let total_duration = *duration_mutex.lock().unwrap();
    //     println!(
    //         "Multi-threaded test total duration for transform no capture group: {:?}",
    //         total_duration
    //     );

    //     Ok(())
    // }
}
