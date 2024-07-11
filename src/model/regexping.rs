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

pub(crate) mod custom_functions {
    use std::sync::{Arc, Mutex};

    use regex::Regex;
    use rusqlite::functions::FunctionFlags;

    use crate::model::database::Database;

    pub fn add_custom_functions(database: &Database) -> rusqlite::Result<()> {
        let cached_filter_regex = Arc::new(Mutex::new(Regex::new("").unwrap())); // Initialize with a default regex
        database.connection.create_scalar_function(
            // this is the cached version
            // it is a lot faster.
            // this one is used to filter, to create new tables
            "regexp",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1);

                match text {
                    Ok(text) => {
                        // Check if the regex has changed, and recompile if necessary
                        let mut cached_filter_regex = cached_filter_regex.lock().unwrap();
                        if cached_filter_regex.as_str() != regex_str {
                            *cached_filter_regex = Regex::new(&regex_str).unwrap();
                        }

                        let result = cached_filter_regex.is_match(&text);

                        Ok(result)
                    }

                    Err(e) => match e {
                        rusqlite::Error::InvalidFunctionParameterType(_, _) => Ok(false),
                        _ => Err(e),
                    },
                }
            },
        )?;

        let cached_with_capture_regex = Arc::new(Mutex::new(Regex::new("").unwrap()));
        database.connection.create_scalar_function(
            // this one is used to filter, to create new tables
            "regexp_transform_with_capture_group",
            3,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1);
                let substitution_pattern = ctx.get::<String>(2)?;
                match text {
                    Ok(text) => {
                        let mut cached_with_capture_regex =
                            cached_with_capture_regex.lock().unwrap();
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
                    }
                    _ => Ok(None),
                }
            },
        )?;
        let cached_no_capture_regex = Arc::new(Mutex::new(Regex::new("").unwrap()));
        database.connection.create_scalar_function(
            // this is used to derive a new column
            "regexp_transform_no_capture_group",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1);
                match text {
                    Ok(text) => {
                        // Check if the regex has changed, and recompile if necessary
                        let mut cached_no_capture_regex = cached_no_capture_regex.lock().unwrap();
                        if cached_no_capture_regex.as_str() != regex_str {
                            *cached_no_capture_regex = Regex::new(&regex_str).unwrap();
                        }

                        let result = cached_no_capture_regex.captures(&text);

                        // let result = my_regex.captures(&text);
                        let val = result
                            .and_then(|c| c.get(0))
                            .map(|v| v.as_str().to_string());
                        Ok(val)
                    }
                    Err(e) => Ok(None),
                }
            },
        )
    }
}
