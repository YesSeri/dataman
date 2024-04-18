use crate::error::AppResult;

pub fn build_exact_search_query(
    ordering: &str,
    search_column: &str,
    current_row: u32,
    table_name: &str,
) -> String {
    let query = format!(
        r#"SELECT rownum FROM 
			(SELECT ROW_NUMBER() OVER ({ordering}) AS rownum, "{search_column}" FROM "{table_name}") 
			WHERE "{search_column}" = ? AND rownum > {current_row} LIMIT 1;"#
    );
    query
}

pub(crate) fn build_text_to_int(table_name: &str, column: &str) -> String {
    build_convert_into(table_name, column, "INT")
}

pub(crate) fn build_int_to_text_query(table_name: &str, column: &str) -> String {
    build_convert_into(table_name, column, "TEXT")
}

fn build_convert_into(table_name: &str, column: &str, kind: &str) -> String {
    let derived_column = format!("{kind}_{column}");
    let create_header_query = format!(
        r#"ALTER TABLE "{table_name}" ADD COLUMN "{derived_column}" {kind};
		"#
    );
    let update_query =
        format!(r#"UPDATE "{table_name}" SET "{derived_column}" = CAST("{column}" as {kind});"#);
    let mut queries = String::new();
    queries.push_str(&create_header_query);
    queries.push_str(&update_query);
    queries
}

pub(crate) fn build_delete_column_query(table_name: &str, column: &str) -> String {
    format!(r#"ALTER TABLE "{table_name}" DROP COLUMN "{column}";"#)
}

pub(crate) fn build_rename_column_query(
    table_name: &str,
    column: &str,
    new_column: &str,
) -> String {
    format!(r#"ALTER TABLE "{table_name}" RENAME COLUMN "{column}" TO "{new_column}";"#)
}

pub fn build_histogram_query(column: &str, table_name: &str) -> String {
    String::new()
}
