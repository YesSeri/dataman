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
    use std::time::Instant;

    use super::*;

    #[test]
    fn test_regex() {
        let header = "header";
        let pattern = r#"^(\d+)$"#;
        let table_name = "table";
        let query = regex_filter_query(header, pattern, table_name).unwrap();

        let expected_query = r#"CREATE TABLE "tableRegexFiltered" AS SELECT * FROM "table" WHERE "header" REGEXP '^(\d+)$';"#;

        assert_eq!(query, expected_query);
    }
}
