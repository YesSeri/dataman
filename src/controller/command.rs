use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::direction::Direction;

#[derive(Debug, Clone)]
pub(crate) struct QueuedCommand {
    pub(crate) command: Command,
    pub(crate) inputs: Vec<String>,
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
    RenameTable,
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
            | Command::DeleteTable
            | Command::RegexFilter => true,
            Command::None
            | Command::IllegalOperation
            | Command::Save
            | Command::Quit
            | Command::Move(_) => false,
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
            KeyCode::Char(c) => {
                log::info!("clicked: {c}");
                Command::None
            }
            _ => Command::None,
        }
    }
}

use std::fmt;
impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::None => write!(f, "None"),
            Command::Copy => write!(f, "Copy"),
            Command::RegexTransform => write!(f, "Regex Transform"),
            Command::RegexFilter => write!(f, "Regex Filter"),
            Command::Edit => write!(f, "Edit"),
            Command::SqlQuery => write!(f, "Sql Query"),
            Command::IllegalOperation => write!(f, "Illegal Operation"),
            Command::Quit => write!(f, "Quit"),
            Command::Sort => write!(f, "Sort"),
            Command::Save => write!(f, "Save"),
            Command::Move(dir) => write!(f, "Move {}", dir),
            Command::NextTable => write!(f, "Next Table"),
            Command::PrevTable => write!(f, "Prev Table"),
            Command::ExactSearch => write!(f, "Exact Search"),
            Command::TextToInt => write!(f, "Text to Int"),
            Command::IntToText => write!(f, "Int to Text"),
            Command::DeleteColumn => write!(f, "Delete Column"),
            Command::DeleteTable => write!(f, "Delete Table"),
            Command::RenameTable => write!(f, "Rename Table"),
            Command::RenameColumn => write!(f, "Rename Column"),
        }
    }
}
