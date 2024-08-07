use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::direction::Direction;

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
    EnterMetadataTable,
    RenameColumn,
}
pub enum MetadataTblCommand {
    None,
    IllegalOperation,
    Move(Direction),
    LeaveMetadataTable,
    JoinMark,
    Join,
    Quit,
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
            | Command::EnterMetadataTable
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
            Command::EnterMetadataTable => "Showing table of tables(metadata)".to_string(),
        }
    }
}

impl From<KeyEvent> for Command {
    fn from(key_event: KeyEvent) -> Self {
        match key_event.code {
            KeyCode::Char('t') => Command::RegexTransform,
            KeyCode::Char('e') => Command::Edit,
            KeyCode::Right | KeyCode::Left | KeyCode::Up | KeyCode::Down => {
                if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    match key_event.code {
                        KeyCode::Right => return Command::NextTable,
                        KeyCode::Left => return Command::PrevTable,
                        _ => (),
                    }
                }
                Command::Move(Direction::from(key_event.code))
            }
            KeyCode::Char('w') => Command::Sort,
            KeyCode::Char('a') => Command::Save,
            KeyCode::Char('q') => Command::SqlQuery,
            KeyCode::Char('f') => Command::RegexFilter,
            KeyCode::Char('c') => {
                if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    Command::Quit
                } else {
                    Command::Copy
                }
            }
            KeyCode::Char('s') => {
                if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    Command::Save
                } else {
                    Command::Sort
                }
            }
            KeyCode::Char('/') => Command::ExactSearch,
            KeyCode::Char('#') => Command::TextToInt,
            KeyCode::Char('$') => Command::IntToText,
            KeyCode::Char('X') => Command::DeleteColumn,
            KeyCode::Char('D') => Command::DeleteTable,
            KeyCode::Char('r') => Command::RenameColumn,
            KeyCode::Char('R') => Command::RenameTable,
            KeyCode::Char('m') => Command::MathOperation,
            KeyCode::Char('M') => Command::EnterMetadataTable,
            KeyCode::Char(c) => {
                log::info!("clicked: {c}");
                Command::None
            }
            _ => Command::None,
        }
    }
}

impl From<KeyEvent> for MetadataTblCommand {
    fn from(key_event: KeyEvent) -> Self {
        match key_event.code {
            KeyCode::Right | KeyCode::Left | KeyCode::Up | KeyCode::Down => {
                MetadataTblCommand::Move(Direction::from(key_event.code))
            }
            KeyCode::Char('c') => {
                if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                    MetadataTblCommand::Quit
                } else {
                    MetadataTblCommand::None
                }
            }
            KeyCode::Char(c) => {
                log::info!("clicked: {c}");
                MetadataTblCommand::None
            }
            _ => MetadataTblCommand::None,
        }
    }
}
use std::fmt::{self, write};
impl Command {}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
