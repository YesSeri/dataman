use rusqlite::Rows;

use crate::error::{log, AppResult};

pub fn build_regex_filter_query(
    header: &str,
    pattern: &str,
    table_name: &str,
) -> AppResult<String> {
    // create new table with filter applied using create table as sqlite statement.
    regex::Regex::new(pattern)?;
    let select_query = format!("SELECT * FROM `{table_name}` WHERE `{header}` REGEXP '{pattern}'");
    let create_table_query = format!("CREATE TABLE `{table_name}RegexFiltered` AS {select_query};");
    Ok(create_table_query)
}

// We need to find the correct query and then find the row it is at. Then we move the row_offset and row_limit to that row.
// TODO this is not working yet.
pub fn build_regex_search_query_find_row_number(
    is_asc_order: bool,
    order_column: &str,
    header: &str,
    pattern: &str,
    table_name: &str,
) -> AppResult<String> {
    // SELECT * FROM (SELECT ROW_NUMBER() OVER (ORDER BY dm.age) as num, * FROM dataMid as dm ORDER BY age) as dm WHERE dm.lastname = 'zenkert';
    //
    // create new table with filter applied using create table as sqlite statement.
    regex::Regex::new(pattern)?;
    let order = if is_asc_order { "ASC" } else { "DESC" };

    let subquery_name = format!("rgxsearch{}", table_name);
    let query = format!("\
    SELECT `{subquery_name}`.rownum FROM \
        (SELECT ROW_NUMBER() OVER (ORDER BY age) as rownum, `{header}` \
        FROM `{table_name}` ORDER BY `{order_column}` {order} LIMIT 1) AS `{subquery_name}` \
    WHERE `{header}` REGEXP '{pattern}';");
    Ok(query)
}

pub(crate) fn build_regex_with_capture_group_transform_query(
    header: &str,
    pattern: &str,
    transformation: String,
    table_name: &str,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    // for each row in the table, run fun on the value of column name and insert the result into the new column
    let derived_header_name = format!("derived{}", header);
    let create_header_query =
        format!("ALTER TABLE `{table_name}` ADD COLUMN `{derived_header_name}` TEXT;\n");

    let mut queries = String::new();
    queries.push_str(&create_header_query);
    log(format!("with capture group queries: {}", queries));
    Ok(queries)
}

pub(crate) fn build_regex_no_capture_group_transform_query(
    header: &str,
    pattern: &str,
    table_name: &str,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    // for each row in the table, run fun on the value of column name and insert the result into the new column
    let derived_header_name = format!("derived{}", header);
    let create_header_query =
        format!("ALTER TABLE `{table_name}` ADD COLUMN `{derived_header_name}` TEXT;\n");

    let mut queries = String::new();
    queries.push_str(&create_header_query);
    let update_query = format!(
        "UPDATE `{table_name}` \
                SET `{derived_header_name}` 
				= regexp_transform_no_capture_group('{pattern}', `{header}`) \
                WHERE id IN (SELECT id FROM `{table_name}` WHERE `{header}` REGEXP '{pattern}');\n"
    );

    queries.push_str(&update_query);
    log(format!("no capture group queries: {}", queries));
    Ok(queries)
}

pub(crate) mod custom_functions {
    use std::{
        cell::RefCell,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use regex::Regex;
    use rusqlite::functions::{Context, FunctionFlags};

    use crate::{error::log, model::database::Database};

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
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex_str = ctx.get::<String>(0)?;

                let text = ctx.get::<String>(1);
                match text {
                    Ok(text) => {
                        // Check if the regex has changed, and recompile if necessary
                        let mut cached_with_capture_regex =
                            cached_with_capture_regex.lock().unwrap();
                        if cached_with_capture_regex.as_str() != regex_str {
                            *cached_with_capture_regex = Regex::new(&regex_str).unwrap();
                        }

                        todo!();
                        // let result = cached_with_capture_regex.captures(&text);
                        // let result = regex::Regex::new(&regex_str).unwrap().is_match(&text);
                        // let result = regex.is_match(&text);
                        // Ok(result)
                    }
                    Err(e) => match e {
                        rusqlite::Error::InvalidFunctionParameterType(_, _) => Ok(false),
                        _ => Err(e),
                    },
                }
            },
        )?;
        // my_regex = Regex::new(r"hen").unwrap();
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
                    Err(e) => match e {
                        // return null
                        rusqlite::Error::InvalidFunctionParameterType(_, _) => Ok(None),
                        _ => Err(e),
                    },
                }
            },
        )
    }
    // fn regex_replace(ctx: &Context) -> rusqlite::Result<String> {
    //     let regex = Regex::new(&ctx.get::<String>(0)?).unwrap();
    //     let text = ctx.get::<String>(1)?;
    //     let transform = ctx.get::<String>(2)?;
    //     let result = regex.replace("Springsteen, Bruce", transform);
    //     Ok(result.to_string())
    // }
    // fn regex_transform(ctx: &Context) -> rusqlite::Result<Option<String>> {
    //     let regex = ctx.get::<String>(0)?;
    //     let text = ctx.get::<String>(1)?;
    //     let result = regex::Regex::new(&regex).unwrap().captures(&text);
    //     let val = result
    //         .and_then(|c| c.get(0))
    //         .map(|v| v.as_str().to_string());
    //     Ok(val)
    // }
}
