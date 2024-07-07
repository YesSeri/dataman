use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use log::info;

use super::direction::Direction;

pub(crate) enum CommandInput {
    Single(String),
    Double(String, String),
}
#[derive(Debug, Clone)]
pub(crate) struct CommandWrapper {
    pub(super) command: Command,
    pub(super) message: Option<String>,
}

impl CommandWrapper {
    pub(crate) fn new(command: Command, message: Option<String>) -> Self {
        Self { command, message }
    }
}

impl std::fmt::Display for CommandWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.message.clone() {
            Some(msg) => write!(f, "{:?}: {}", self.command, msg),
            None => write!(f, "{:?}", self.command),
        }
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
            | Command::RenameColumn
            | Command::ExactSearch
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
            KeyCode::Char('r') => Command::RegexTransform,
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
            KeyCode::Char('R') => Command::RenameColumn,
            KeyCode::Char(c) => {
                info!("clicked: {c}");
                Command::None
            }
            _ => Command::None,
        }
    }
}
