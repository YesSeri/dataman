use std::f32::consts::E;
use std::path::Path;
use std::{
    fmt::format,
    io::{self, Error, Read, Write},
    path::PathBuf,
    thread::sleep,
    time::Duration,
};

use crossterm::{
    event::{poll, read, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal, ExecutableCommand,
};
use ratatui::widgets::TableState;
use regex::Regex;
use rusqlite::Connection;

use crate::model::datarow::{DataRow, DataTable};
use crate::{error::AppResult, model::database::Database};
use crate::{
    error::{log, AppError},
    tui::TUI,
};

#[derive(Debug, Clone)]
pub struct CommandWrapper {
    command: Command,
    message: Option<String>,
}

impl CommandWrapper {
    pub fn new(command: Command, message: Option<String>) -> Self {
        Self { command, message }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    None,
    Copy,
    RegexTransform,
    Edit,
    SqlQuery,
    IllegalOperation,
    Quit,
    Sort,
    Save,
    Move(Direction),
    RegexFilter,
    NextTable,
    PrevTable,
    // RegexTransform,
    RegexSearch,
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
            KeyCode::Char('/') => Command::RegexSearch,
            _ => Command::None,
        }
    }
}

impl From<KeyCode> for Direction {
    fn from(value: KeyCode) -> Self {
        match value {
            KeyCode::Right => Direction::Right,
            KeyCode::Left => Direction::Left,
            KeyCode::Up => Direction::Up,
            KeyCode::Down => Direction::Down,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl std::fmt::Display for CommandWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.message.clone() {
            Some(msg) => write!(f, "{:?}{}", self.command, msg),
            None => write!(f, "{:?}", self.command),
        }
    }
}

#[derive(Debug)]
pub struct Controller {
    pub ui: TUI,
    pub database: Database,
    pub last_command: CommandWrapper,
}

impl Controller {
    pub(crate) fn save_to_sqlite_file(&self) -> AppResult<()> {
        let filename = TUI::get_editor_input("Enter file name")?;
        let path = PathBuf::from(filename);
        Ok(self.database.backup_db(path)?)
    }

    pub(crate) fn sql_query(&self) -> Result<(), AppError> {
        let query = TUI::get_editor_input("Enter sqlite query")?;
        self.database.sql_query(query)
    }

    pub fn set_last_command(&mut self, last_command: CommandWrapper) {
        self.last_command = last_command;
    }

    pub fn new(ui: TUI, database: Database) -> Self {
        Self {
            ui,
            database,
            last_command: CommandWrapper::new(Command::None, None),
        }
    }
    pub fn start(mut self) -> Result<(), AppError> {
        loop {
            let r = self.run();
            if self.last_command.command == Command::Quit {
                break;
            }
            if let Err(e) = r {
                match e {
                    AppError::Io(_) | AppError::Parse(_) => break,
                    _ => (),
                }
            }
        }

        self.ui.shutdown()
    }

    pub fn run(&mut self) -> Result<(), AppError> {
        let command = TUI::start(self);
        let res = match command {
            Ok(command) => {
                log(format!("\ncommand: {:?}", command));
                let result = match command {
                    Command::Quit => {
                        self.last_command = CommandWrapper::new(Command::Quit, None);
                        Ok(())
                    }
                    Command::Copy => self.copy(),
                    Command::RegexTransform => self.regex_transform(),
                    Command::Edit => self.edit_cell(),
                    Command::SqlQuery => self.sql_query(),
                    Command::IllegalOperation => {
                        self.last_command = CommandWrapper::new(Command::IllegalOperation, None);
                        Ok(())
                    }
                    Command::None => {
                        self.last_command = CommandWrapper::new(Command::None, None);
                        Ok(())
                    }
                    Command::Sort => self.sort(),
                    Command::Save => self.save_to_sqlite_file(),
                    Command::Move(direction) => {
                        self.database.move_cursor(direction)?;
                        Ok(())
                    }
                    Command::RegexFilter => self.regex_filter(),
                    Command::NextTable => {
                        self.last_command = CommandWrapper::new(Command::NextTable, None);
                        self.database.next_table()
                    }
                    Command::PrevTable => {
                        self.last_command = CommandWrapper::new(Command::PrevTable, None);
                        self.database.prev_table()
                    }
                    Command::RegexSearch => {
                        self.last_command = CommandWrapper::new(Command::RegexSearch, None);
                        self.regex_search()?;
                        Ok(())
                    }
                };

                match command {
                    Command::Copy
                    | Command::RegexTransform
                    | Command::Edit
                    | Command::SqlQuery
                    | Command::Sort
                    | Command::NextTable
                    | Command::PrevTable
                    | Command::RegexFilter => self.database.current_view.has_changed(),
                    Command::None
                    | Command::IllegalOperation
                    | Command::Save
                    | Command::Quit
                    | Command::RegexSearch
                    | Command::Move(_) => (),
                }
                result
            }
            Err(result) => {
                log(format!("\nAPP ERROR: {:?}", result));
                self.database.current_view.has_changed();

                self.set_last_command(CommandWrapper::new(
                    Command::IllegalOperation,
                    Some(result.to_string()),
                ));
                Ok(())
            }
        };
        if let Err(e) = res {
            log(format!("\nAPP ERROR: {:?}", e));

            self.database.current_view.has_changed();
            self.set_last_command(CommandWrapper::new(
                Command::IllegalOperation,
                Some(format!(": {}", e)),
            ));
        }
        Ok(())
    }
    pub fn get_headers_and_rows(&mut self, limit: u32) -> AppResult<DataTable> {
        let binding = "default table name".to_string();
        let first_table = self.database.get_current_table_name()?;
        self.database.get(limit, 0, first_table)
    }

    pub fn regex_filter(&mut self) -> AppResult<()> {
        let pattern = TUI::get_editor_input("Enter regex")?;
        log(format!("pattern: {:?}", pattern));
        let header = self.database.get_current_header()?;
        self.database.regex_filter(&header, &pattern)?;

        Ok(())
    }

    /// The user enters a regex and we will derive a new column from that.
    /// If the user enter a capture group we will ask for a second input that shows how the capture group should be transformed.
    /// If the user doesn't enter a capture group we will just copy the regex match.
    pub fn regex_transform(&mut self) -> AppResult<()> {
        let pattern =
            TUI::get_editor_input(r"Enter regex, e.g. (?<last>[^,\s]+),\s+(?<first>\S+)")?;
        let regex = regex::Regex::new(&pattern)?;
        let contains_capture_pattern = regex.capture_names().len() > 1;
        let header = self.database.get_current_header()?;
        if contains_capture_pattern {
            let transformation =
                TUI::get_editor_input(r"Enter transformation, e.g. '$first $last'")?;

            log(format!(
                "pattern: {pattern:?}, transformation: {transformation:?}"
            ));

            self.database
                .regex_capture_group_transform(&pattern, &header, transformation)?;
        } else {
            log(format!("pattern: {pattern:?}"));
            self.database
                .regex_no_capture_group_transform(&pattern, &header)?;
        }

        Ok(())
    }

    pub fn copy(&mut self) -> AppResult<()> {
        self.database.copy()?;
        self.set_last_command(CommandWrapper::new(Command::Copy, None));
        Ok(())
    }

    pub(crate) fn edit_cell(&mut self) -> AppResult<()> {
        let header = self.database.get_current_header()?;
        log(format!("header: {:?}", header));
        let id = self.database.get_current_id()?;
        log(format!("id: {:?}", id));
        let data = self.database.get_cell(id, &header)?;
        log(format!("data: {:?}", data));

        let result = TUI::get_editor_input(&data)?;
        self.database.update_cell(header.as_str(), id, &result)?;
        Ok(())
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        self.database.sort()
    }
    fn regex_search(&mut self) -> AppResult<()> {
        let pattern = TUI::get_editor_input("Enter regex")?;
        log(format!("pattern: {:?}", pattern));
        let header = self.database.get_current_header()?;
        self.database.regex_search(&header, &pattern).unwrap();
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use super::*;

    #[test]
    fn copy_column_test() {
        let p = Path::new("assets/data.csv");
        let mut database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| Some(s.to_string());

        database.move_cursor(Direction::Right).unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let (_, res) = database.get(20, 100, "data".to_string()).unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_long_test() {
        let p = Path::new("assets/data-long.csv");
        let mut database = Database::try_from(p).unwrap();

        let copy_fun = |s: String| Some(s.to_string());
        database.move_cursor(Direction::Right).unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let table_name = database.get_current_table_name().unwrap();
        let (_, res) = database.get(20, 0, table_name).unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }
}
