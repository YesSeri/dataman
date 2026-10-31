//! Stable names used by automation; the enum remains the execution backend.
use crate::controller::Command;
use crate::navigation::Direction;
use serde_json::{json, Value};
pub fn resolve(name: &str) -> Result<Command, String> {
    Ok(match name {
        "quit" => Command::Quit,
        "copy" => Command::Copy,
        "regex_transform" => Command::RegexTransform,
        "regex_filter" => Command::RegexFilter,
        "edit" => Command::Edit,
        "sql_query" => Command::SqlQuery,
        "sort" => Command::Sort,
        "save" => Command::Save,
        "next_table" => Command::NextTable,
        "previous_table" => Command::PrevTable,
        "previous_sheet" => Command::PreviousSheet,
        "exact_search" => Command::ExactSearch,
        "text_to_int" => Command::TextToInt,
        "int_to_text" => Command::IntToText,
        "delete_column" => Command::DeleteColumn,
        "delete_table" => Command::DeleteTable,
        "math_operation" => Command::MathOperation,
        "rename_table" => Command::RenameTable,
        "rename_column" => Command::RenameColumn,
        "metadata" => Command::ToggleMetadataTable,
        "undo" => Command::Undo,
        "redo" => Command::Redo,
        "summary" => Command::Summary,
        "frequency" => Command::Frequency,
        "join" => Command::Join,
        "combine_tables" => Command::CombineTables,
        "toggle_join_key" => Command::ToggleJoinKey,
        "configure_aggregator" => Command::ConfigureAggregator,
        "group" => Command::Group,
        "pivot" => Command::Pivot,
        "refresh_derived" => Command::RefreshDerived,
        "toggle_selection" => Command::ToggleSelection,
        "bulk_edit" => Command::BulkEdit,
        "hide_column" => Command::HideColumn,
        "show_columns" => Command::ShowColumns,
        "column_left" => Command::ColumnLeft,
        "column_right" => Command::ColumnRight,
        "history" => Command::History,
        "history_replay" => Command::HistoryReplay,
        "history_close" => Command::HistoryClose,
        "select_sql" => Command::SelectSql,
        "select_regex" => Command::SelectRegex,
        "delete_selected" => Command::DeleteSelected,
        "copy_selected" => Command::CopySelected,
        "clear_selection" => Command::ClearSelection,
        "move_left" => Command::Move(Direction::Left),
        "move_right" => Command::Move(Direction::Right),
        "move_up" => Command::Move(Direction::Up),
        "move_down" => Command::Move(Direction::Down),
        "invert_selection" => Command::InvertSelection,
        _ => return Err(format!("unknown command: {name}")),
    })
}
pub fn names() -> Vec<&'static str> {
    vec![
        "quit",
        "copy",
        "regex_transform",
        "regex_filter",
        "edit",
        "sql_query",
        "sort",
        "save",
        "next_table",
        "previous_table",
        "previous_sheet",
        "exact_search",
        "text_to_int",
        "int_to_text",
        "delete_column",
        "delete_table",
        "math_operation",
        "rename_table",
        "rename_column",
        "metadata",
        "undo",
        "redo",
        "summary",
        "frequency",
        "join",
        "combine_tables",
        "toggle_join_key",
        "configure_aggregator",
        "group",
        "pivot",
        "refresh_derived",
        "toggle_selection",
        "bulk_edit",
        "hide_column",
        "show_columns",
        "column_left",
        "column_right",
        "history",
        "history_replay",
        "history_close",
        "select_sql",
        "select_regex",
        "delete_selected",
        "copy_selected",
        "clear_selection",
        "move_left",
        "move_right",
        "move_up",
        "move_down",
        "invert_selection",
    ]
}
pub fn longname(name: &str) -> Option<&'static str> {
    match name {
        "invert_selection" => Some("stoggle-rows"),
        "clear_selection" => Some("unselect-rows"),
        _ => None,
    }
}
pub fn describe() -> Value {
    json!({"schema_version":1,"commands":names().iter().map(|name| {
        let c = resolve(name).unwrap();
        json!({"name":name,"prompt":if c.needs_input(){Some(c.prompt())}else{None},
            "parameters":if c.needs_input(){"string inputs"}else{"none"},"visidata_longname":longname(name),
            "profile_bindings":(["visidata","dataman"].into_iter().map(|profile| {
                let dispatcher=super::interaction::Dispatcher::new(profile,false);
                let mut keys=dispatcher.bindings().iter().filter(|(_,v)|v.as_str()==*name).map(|(k,_)|k.clone()).collect::<Vec<_>>();keys.sort();(profile,keys)
            }).collect::<std::collections::BTreeMap<_,_>>())})
    }).collect::<Vec<_>>()})
}
pub fn validate(command: &Command, inputs: &[String]) -> Result<(), String> {
    command.validate_inputs(inputs).map_err(|e| e.to_string())?;
    let max = if *command == Command::RegexTransform {
        2
    } else if command.needs_input() {
        1
    } else {
        0
    };
    if inputs.len() > max {
        return Err(format!("expected at most {max} inputs"));
    }
    Ok(())
}
