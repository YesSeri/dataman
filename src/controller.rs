use std::path::Path;
use std::{
    io::{Read, Write},
    path::PathBuf,
};

use crossterm::event::{self, Event};
use crossterm::{
    event::{KeyCode, KeyEvent, KeyModifiers},
    ExecutableCommand,
};
use log::{error, info};

use crate::error::AppError;
use crate::model::datarow::DataTable;
use crate::tui::TUI;
use crate::Config;
use crate::{error::AppResult, model::database::Database};

#[derive(Debug, Clone)]
pub(crate) struct CommandWrapper {
    command: Command,
    message: Option<String>,
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
    Join(Join),
}

#[derive(Debug, PartialEq, Clone)]
pub struct Join {
    tables: (String, String),
    conditions: Vec<(String, String)>, // Pairs of column names to join on
    join_type: JoinType,
}
//
//impl Clone for Join {
//fn clone(&self) -> Self {
//let conditions: Vec<(String, String)> =
//self.conditions.iter().map(|c| (c.0.clone(), c.1.clone()));
//Self {
//tables: (self.tables.0.clone(), self.tables.0.clone()),
//conditions,
//join_type: self.join_type,
//}
//}
//}

/// Outer Join is Left Outer Join
/// Cross Join is cartesian product of the two tables.
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum JoinType {
    Inner,
    Outer,
    Cross,
}
impl Command {
    fn requires_updating_view(&self) -> bool {
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
            Command::Join(_) => todo!(),
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

#[derive(Debug)]
pub struct Controller {
    pub(crate) ui: TUI,
    pub(crate) database: Database,
    pub(crate) last_command: CommandWrapper,
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

    pub(crate) fn set_last_command(&mut self, last_command: CommandWrapper) {
        self.last_command = last_command;
    }

    pub fn new(ui: TUI, database: Database) -> Self {
        Self {
            ui,
            database,
            last_command: CommandWrapper::new(Command::None, None),
        }
    }

    pub fn run(&mut self) -> AppResult<()> {
        loop {
            TUI::draw(self)?;
            if event::poll(std::time::Duration::from_millis(3000))? {
                let res = match if let Event::Key(key) = event::read()? {
                    Ok(Command::from(key))
                } else {
                    Err(AppError::Other)
                } {
                    Ok(command) => {
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
                                self.last_command =
                                    CommandWrapper::new(Command::IllegalOperation, None);
                                Ok(())
                            }
                            Command::None => {
                                self.last_command = CommandWrapper::new(Command::None, None);
                                Ok(())
                            }
                            Command::Sort => self.sort(),
                            Command::Save => self.save_to_sqlite_file(),
                            Command::Move(direction) => self.database.move_cursor(direction),
                            Command::RegexFilter => self.regex_filter(),
                            Command::NextTable => {
                                self.last_command = CommandWrapper::new(Command::NextTable, None);
                                self.database.next_table()
                            }
                            Command::PrevTable => {
                                self.last_command = CommandWrapper::new(Command::PrevTable, None);
                                self.database.prev_table()
                            }
                            Command::ExactSearch => self.exact_search(),

                            Command::TextToInt => self.text_to_int(),
                            Command::IntToText => self.int_to_text(),
                            Command::DeleteColumn => self.delete_column(),
                            Command::RenameColumn => self.rename_column(),
                            Command::Join(_) => todo!(),
                        };

                        if command.requires_updating_view() {
                            self.database.slices[0].has_changed();
                        }
                        result
                    }
                    Err(result) => {
                        error!("\nAPP ERROR: {:?}", result);
                        self.database.slices[0].has_changed();

                        self.set_last_command(CommandWrapper::new(
                            Command::IllegalOperation,
                            Some(result.to_string()),
                        ));
                        Ok(())
                    }
                };
                if let Err(e) = res {
                    error!("\nAPP ERROR: {:?}", e);

                    self.database.slices[0].has_changed();
                    self.set_last_command(CommandWrapper::new(
                        Command::IllegalOperation,
                        Some(format!(": {}", e)),
                    ));
                }

                if self.last_command.command == Command::Quit {
                    self.ui.shutdown()?;
                    break Ok(());
                }
            }
        }
    }
    pub fn get_headers_and_rows(&mut self, limit: u32) -> AppResult<DataTable> {
        let binding = "default table name".to_string();
        let first_table = self.database.get_current_table_name()?;
        self.database.get(limit, 0, first_table)
    }

    pub fn regex_filter(&mut self) -> AppResult<()> {
        let pattern = TUI::get_editor_input("Enter regex")?;
        info!("pattern: {:?}", pattern);
        let header = self.database.get_current_header()?;
        self.database.regex_filter(&header, &pattern)?;

        Ok(())
    }

    /// The user enters a regex and we will derive a new column from that.
    /// If the user enter a capture group we will ask for a second input that shows how the capture group should be transformed.
    /// If the user doesn't enter a capture group we will just copy the regex match.
    pub fn regex_transform(&mut self) -> AppResult<()> {
        let pattern = if cfg!(debug_assertions) {
            TUI::get_editor_input(r"(u).*(.)")?
        } else {
            TUI::get_editor_input(r"Enter regex, e.g. (?<last>[^,\s]+),\s+(?<first>\S+)")?
        };
        let regex = regex::Regex::new(&pattern)?;
        let contains_capture_pattern = regex.capture_names().len() > 1;
        let header = self.database.get_current_header()?;
        if contains_capture_pattern {
            let transformation = if cfg!(debug_assertions) {
                TUI::get_editor_input(r"${1}${2}")?
            } else {
                TUI::get_editor_input(
                    r"Enter transformation, e.g. '${first} ${second}' or '${1} ${2}' if un-named",
                )?
            };
            info!("pattern: {pattern:?}, transformation: {transformation:?}");

            self.database
                .regex_capture_group_transform(&pattern, &header, &transformation)?;
        } else {
            info!("pattern: {pattern:?}");
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
        info!("header: {:?}", header);
        let id = self.database.get_current_id()?;
        info!("id: {:?}", id);
        let data = self.database.get_cell(id, &header)?;
        info!("data: {:?}", data);

        let result = TUI::get_editor_input(&data)?;
        self.database.update_cell(header.as_str(), id, &result)?;
        Ok(())
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        self.database.sort()
    }
    fn exact_search(&mut self) -> AppResult<()> {
        let pattern = TUI::get_editor_input("Enter regex")?;
        info!("pattern: {:?}", pattern);
        let header = self.database.get_current_header()?;
        match self.database.exact_search(&header, &pattern) {
            Ok(_) => {
                self.last_command =
                    CommandWrapper::new(Command::ExactSearch, Some("Match found".to_string()));
            }
            Err(_) => {
                self.last_command =
                    CommandWrapper::new(Command::ExactSearch, Some("No match found".to_string()));
            }
        }
        Ok(())
    }

    fn text_to_int(&mut self) -> AppResult<()> {
        self.last_command = CommandWrapper::new(Command::TextToInt, None);
        self.database.text_to_int()
    }

    fn int_to_text(&mut self) -> AppResult<()> {
        self.last_command = CommandWrapper::new(Command::IntToText, None);
        self.database.int_to_text()
    }

    fn delete_column(&mut self) -> Result<(), AppError> {
        let text = self.database.delete_column()?;
        self.last_command = CommandWrapper::new(Command::DeleteColumn, text);
        Ok(())
    }

    fn rename_column(&mut self) -> Result<(), AppError> {
        let new_column = TUI::get_editor_input("Enter new column name.")?;
        self.database.rename_column(&new_column)?;
        self.last_command = CommandWrapper::new(Command::RenameColumn, None);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn copy_column_test() {
        let p = PathBuf::from("assets/data.csv");
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
        let p = PathBuf::from("assets/data-long.csv");
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
