use crate::error::AppResult;

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn regex_filter_query(
    header: &str,
    pattern: &str,
    old_table_name: &str,
    new_table_name: &str,
) -> AppResult<(String, String)> {
    // create new table with filter applied using create table as sqlite statement.
    regex::Regex::new(pattern)?;
    let select_query = format!(
        "SELECT * FROM {} WHERE {} REGEXP {}",
        quote_identifier(old_table_name),
        quote_identifier(header),
        quote_literal(pattern)
    );

    let create_table_query = format!(
        "CREATE TABLE {} AS {select_query};",
        quote_identifier(new_table_name)
    );
    Ok((create_table_query, new_table_name.to_owned()))
}

pub(crate) fn regex_with_capture_group_transform_query(
    old_header: &str,
    new_header: &str,
    pattern: &str,
    transformation: &str,
    table_name: &str,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    let mut queries = format!(
        "ALTER TABLE {} ADD COLUMN {} TEXT;",
        quote_identifier(table_name),
        quote_identifier(new_header)
    );
    let update_query = format!(
        "UPDATE {} SET {} = regexp_transform_with_capture_group({}, {}, {});",
        quote_identifier(table_name),
        quote_identifier(new_header),
        quote_literal(pattern),
        quote_identifier(old_header),
        quote_literal(transformation)
    );
    queries.push_str(&update_query);
    Ok(queries)
}

pub(crate) fn regex_no_capture_group_transform_query(
    old_header: &str,
    new_header: &str,
    pattern: &str,
    table_name: &str,
) -> AppResult<String> {
    regex::Regex::new(pattern)?;
    // for each row in the table, run fun on the value of column name and insert the result into the new column
    let mut queries = format!(
        "ALTER TABLE {} ADD COLUMN {} TEXT;",
        quote_identifier(table_name),
        quote_identifier(new_header)
    );
    let update_query = format!(
        "UPDATE {} SET {} = regexp_transform_no_capture_group({}, {});",
        quote_identifier(table_name),
        quote_identifier(new_header),
        quote_literal(pattern),
        quote_identifier(old_header)
    );
    queries.push_str(&update_query);
    Ok(queries)
}

pub mod custom_functions {
    use regex::Regex;
    use rusqlite::{
        functions::{Context, FunctionFlags},
        types::ValueRef,
        Connection,
    };
    use std::sync::Arc;
    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    fn compiled(ctx: &Context<'_>) -> rusqlite::Result<Arc<Regex>> {
        ctx.get_or_create_aux(0, |value| -> Result<Regex, BoxError> {
            Ok(Regex::new(value.as_str()?)?)
        })
    }
    fn text<'a>(ctx: &'a Context<'_>) -> rusqlite::Result<Option<&'a str>> {
        match ctx.get_raw(1) {
            ValueRef::Null => Ok(None),
            value => value
                .as_str()
                .map(Some)
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into())),
        }
    }
    pub fn add_custom_functions(conn: &Connection) -> rusqlite::Result<()> {
        let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
        conn.create_scalar_function("regexp", 2, flags, |ctx| {
            let regex = compiled(ctx)?;
            Ok(text(ctx)?.is_some_and(|value| regex.is_match(value)))
        })?;
        conn.create_scalar_function("regexp_transform_with_capture_group", 3, flags, |ctx| {
            let regex = compiled(ctx)?;
            let Some(value) = text(ctx)? else {
                return Ok(None);
            };
            let template = ctx
                .get_raw(2)
                .as_str()
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
            Ok(regex.captures(value).map(|captures| {
                let mut result = String::new();
                result.push_str(&value[..captures.get(0).unwrap().start()]);
                captures.expand(template, &mut result);
                result.push_str(&value[captures.get(0).unwrap().end()..]);
                result
            }))
        })?;
        conn.create_scalar_function("regexp_transform_no_capture_group", 2, flags, |ctx| {
            let regex = compiled(ctx)?;
            Ok(text(ctx)?.and_then(|value| regex.find(value).map(|m| m.as_str().to_owned())))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn test_regex() {
        let header = "header";
        let header_new = "header";
        let pattern = r#"^(\d+)$"#;
        let table_name = "table";
        let (query, _) =
            regex_filter_query(header, pattern, table_name, "tableRegexFiltered").unwrap();

        let expected_query = r#"CREATE TABLE "tableRegexFiltered" AS SELECT * FROM "table" WHERE "header" REGEXP '^(\d+)$';"#;

        assert_eq!(query, expected_query);
    }
}
