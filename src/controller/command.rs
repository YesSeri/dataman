use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::navigation::Direction;

#[derive(Debug, Clone)]
pub(crate) struct QueuedCommand {
    pub(super) command: Command,
    pub(super) inputs: Vec<String>,
}

impl QueuedCommand {
    pub(crate) fn new(command: Command) -> Self {
        Self {
            command,
            inputs: vec![],
        }
    }
}
impl Default for QueuedCommand {
    fn default() -> Self {
        QueuedCommand::new(Command::None)
    }
}
#[derive(Debug, Clone)]
pub(crate) struct PreviousCommand {
    pub(crate) command: Command,
    pub(super) message: Option<String>,
}

impl PreviousCommand {
    pub(crate) fn new(command: Command, message: Option<String>) -> Self {
        Self { command, message }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    None,
    Copy,
    RegexTransform,
    RegexFilter,
    Edit,
    SqlQuery,
    IllegalOperation,
    Quit,
    Sort,
    Save,
    Move(Direction),
    NextTable,
    PrevTable,
    ExactSearch,
    TextToInt,
    IntToText,
    DeleteColumn,
    DeleteTable,
    MathOperation,
    RenameTable,
    ToggleMetadataTable,
    RenameColumn,
}

impl Command {
    pub fn requires_updating_view(&self) -> bool {
        match self {
            Command::Copy
            | Command::RegexTransform
            | Command::Edit
            | Command::SqlQuery
            | Command::Sort
            | Command::NextTable
            | Command::PrevTable
            | Command::TextToInt
            | Command::IntToText
            | Command::DeleteColumn
            | Command::RenameTable
            | Command::RenameColumn
            | Command::ExactSearch
            | Command::ToggleMetadataTable
            | Command::DeleteTable
            | Command::MathOperation
            | Command::RegexFilter => true,
            Command::None
            | Command::IllegalOperation
            | Command::Save
            | Command::Quit
            | Command::Move(_) => false,
        }
    }
    fn as_str(&self) -> String {
        match self {
            Command::None => "None".to_string(),
            Command::Copy => "Copy".to_string(),
            Command::RegexTransform => "Regex Transform".to_string(),
            Command::RegexFilter => "Regex Filter".to_string(),
            Command::Edit => "Edit".to_string(),
            Command::SqlQuery => "Sql Query".to_string(),
            Command::IllegalOperation => "Illegal Operation".to_string(),
            Command::Quit => "Quit".to_string(),
            Command::Sort => "Sort".to_string(),
            Command::Save => "Save".to_string(),
            Command::Move(dir) => format!("Move {}", dir),
            Command::NextTable => "Next Table".to_string(),
            Command::PrevTable => "Prev Table".to_string(),
            Command::ExactSearch => "Exact Search".to_string(),
            Command::TextToInt => "Text to Int".to_string(),
            Command::IntToText => "Int to Text".to_string(),
            Command::DeleteColumn => "Delete Column".to_string(),
            Command::MathOperation => "Math Operation".to_string(),
            Command::DeleteTable => "Delete Table".to_string(),
            Command::RenameTable => "Rename Table".to_string(),
            Command::RenameColumn => "Rename Column".to_string(),
            Command::ToggleMetadataTable => "Showing table of tables(metadata)".to_string(),
        }
    }
}

impl From<KeyEvent> for Command {
    fn from(key_event: KeyEvent) -> Self {
        let key = normalized_key(&key_event);
        let command_name = crate::KEYBINDINGS.bindings.get(&key);
        let Some(command_name) = command_name else {
            return Command::None;
        };
        match command_name.as_str() {
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
            "exact_search" => Command::ExactSearch,
            "text_to_int" => Command::TextToInt,
            "int_to_text" => Command::IntToText,
            "delete_column" => Command::DeleteColumn,
            "delete_table" => Command::DeleteTable,
            "math_operation" => Command::MathOperation,
            "rename_table" => Command::RenameTable,
            "rename_column" => Command::RenameColumn,
            "metadata" => Command::ToggleMetadataTable,
            "move_left" => Command::Move(Direction::Left),
            "move_right" => Command::Move(Direction::Right),
            "move_up" => Command::Move(Direction::Up),
            "move_down" => Command::Move(Direction::Down),
            _ => Command::None,
        }
    }
}

fn normalized_key(key: &KeyEvent) -> String {
    let key_name = match key.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Right => "right".to_owned(),
        KeyCode::Left => "left".to_owned(),
        KeyCode::Up => "up".to_owned(),
        KeyCode::Down => "down".to_owned(),
        KeyCode::Enter => "enter".to_owned(),
        KeyCode::Esc => "esc".to_owned(),
        _ => return String::new(),
    };
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        format!("ctrl+{key_name}")
    } else {
        key_name
    }
}

use std::fmt::{self, write};
impl Command {}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod keybinding_tests {
    use super::*;

    #[test]
    fn configured_keys_preserve_current_runtime_commands() {
        assert_eq!(
            Command::from(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Command::SqlQuery
        );
        assert_eq!(
            Command::from(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Command::Quit
        );
        assert_eq!(
            Command::from(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL)),
            Command::NextTable
        );
    }
}
