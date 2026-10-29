pub(crate) fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) mod build {
    use super::quote_identifier;

    pub(crate) fn rename_column_query(table: &str, column: &str, new_column: &str) -> String {
        format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {};",
            quote_identifier(table),
            quote_identifier(column),
            quote_identifier(new_column)
        )
    }

    pub(crate) fn rename_table_query(old: &str, new: &str) -> String {
        format!(
            "ALTER TABLE {} RENAME TO {};",
            quote_identifier(old),
            quote_identifier(new)
        )
    }
}
