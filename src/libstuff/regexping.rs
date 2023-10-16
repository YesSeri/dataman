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

pub(crate) fn build_regex_transform_query(
    header: &str,
    pattern: &str,
    transformation: Option<String>,
    table_name: &str,
    rows: &mut Rows,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    // for each row in the table, run fun on the value of column name and insert the result into the new column
    let derived_header_name = format!("derived{}", header);
    let create_header_query =
        format!("ALTER TABLE `{table_name}` ADD COLUMN `{derived_header_name}` TEXT;");

    let mut queries = String::new();
    queries.push_str(&create_header_query);
    // TODO use a transaction
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let value: String = row.get(1)?;
        // let derived_value = fun(value).unwrap_or("NULL".to_string());
        let update_query = if let Some(ref transformation) = transformation {
            format!(
                "UPDATE `{table_name}` SET '{derived_header_name}' = regexp_transform('{value}', '{pattern}', '{transformation}') WHERE id = '{id}';",
            )
        } else {
            format!(
                "UPDATE `{table_name}` SET '{derived_header_name}' = regexp_simple('{value}', '{pattern}') WHERE id = '{id}';",
            )
        };
        queries.push_str(&update_query);
    }
    log(format!("queries: {:?}", queries));
    Ok(queries)
}

pub(crate) mod custom_functions {
    use regex::Regex;
    use rusqlite::functions::{Context, FunctionFlags};

    use crate::{error::log, libstuff::db::Database};

    pub fn add_custom_functions(database: &Database) -> rusqlite::Result<()> {
        let mut my_regex = Regex::new(r"hen").unwrap();
        database.connection.create_scalar_function(
            // this one is used to filter, to create new tables
            "regexp_filter",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                // let regex = ctx.get::<String>(0)?;
                let regex = &my_regex;
                let text = ctx.get::<String>(1)?;
                // let result = regex::Regex::new(&regex).unwrap().is_match(&text);
                let result = regex.is_match(&text);
                Ok(result)
            },
        )?;
        // my_regex = Regex::new(r"hen").unwrap();
        database.connection.create_scalar_function(
            // this is used to derive a new column
            "regexp_simple",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |ctx| {
                let regex = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1)?;
                let result = regex::Regex::new(&regex).unwrap().captures(&text);
                // let result = my_regex.captures(&text);
                let val = result
                    .and_then(|c| c.get(0))
                    .map(|v| v.as_str().to_string());
                Ok(val)
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
