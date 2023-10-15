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

use crate::{error::AppResult, libstuff::db::Database};
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

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    None,
    Copy,
    Regex,
    Edit,
    SqlQuery,
    IllegalOperation,
    Quit,
    Sort,
    Save,
    Move(Direction),
    RegexFilter,
}
impl From<KeyEvent> for Command {
    fn from(key_event: KeyEvent) -> Self {
        match key_event.code {
            KeyCode::Char('r') => Command::Regex,
            KeyCode::Char('e') => Command::Edit,
            KeyCode::Right | KeyCode::Left | KeyCode::Up | KeyCode::Down => {
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

#[derive(Debug, Clone, PartialEq)]
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
pub struct Controller {
    pub ui: TUI,
    pub database: Database,
    pub last_command: CommandWrapper,
}

impl Controller {
    pub(crate) fn save(&self) -> AppResult<()> {
        log("save not implemented".to_owned());
        Ok(())
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
            log(format!("last cmd r: {:?}", self.last_command.command));
            if self.last_command.command == Command::Quit {
                break;
            }
            match r {
                Ok(_) => {}
                Err(e) => {
                    // if csv parse error the abort
                    if let AppError::Parse(_) = e {
                        break;
                    }

                    if let AppError::Io(_) = e {
                        break;
                    }
                }
            }
        }

        self.ui.shutdown()
    }

    pub fn run(&mut self) -> Result<(), AppError> {
        let res = TUI::start(self);
        log(format!("res: {:?}", res));
        match res {
            Ok(command) => match command {
                Command::Quit => {
                    self.last_command = CommandWrapper::new(Command::Quit, None);
                    Ok(())
                }
                Command::Copy => self.copy(),
                Command::Regex => self.regex_filter(),
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
                Command::Sort => todo!(),
                Command::Save => todo!(),
                Command::Move(direction) => {
                    let height = self.ui.get_terminal_height()?;
                    self.database.move_cursor(direction, height)?;
                    Ok(())
                }
                Command::RegexFilter => todo!(),
            },
            Err(result) => {
                self.set_last_command(CommandWrapper::new(
                    Command::IllegalOperation,
                    Some(result.to_string()),
                ));
                Ok(())
            }
        }
    }
    pub fn get_headers_and_rows(
        &mut self,
        limit: i32,
    ) -> AppResult<(Vec<String>, Vec<Vec<String>>)> {
        let binding = "default table name".to_string();
        let first_table = self.database.get_current_table_name()?;
        self.database.get(limit, 0, first_table)
    }

    pub fn regex_filter(&mut self) -> AppResult<()> {
        let pattern = TUI::get_editor_input("Enter regex")?;
        let pattern = pattern.trim_end_matches('\n');
        log(format!("pattern: {:?}", pattern));
        let header = self.database.get_current_header()?;
        self.database.regex_filter(&header, pattern)?;

        Ok(())
    }
    pub fn regex(&mut self) -> AppResult<()> {
        let pattern = TUI::get_editor_input("Enter regex")?;
        // remove last
        let pattern = pattern.trim_end_matches('\n');
        self.set_last_command(CommandWrapper::new(
            Command::Regex,
            Some(pattern.to_string()),
        ));
        let column_name = self.database.get_current_header()?;
        self.database.regex(pattern, column_name)
    }

    pub fn derive_column<F>(&mut self, fun: F) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        let column_name = self.database.get_current_header()?;
        self.database.derive_column(column_name, fun)
    }

    pub fn copy(&mut self) -> AppResult<()> {
        let fun = |s: String| Some(s.to_string());
        self.set_last_command(CommandWrapper::new(Command::Copy, None));
        self.derive_column(fun)?;
        Ok(())
    }

    pub(crate) fn edit_cell(&mut self) -> AppResult<()> {
        let header = self.database.get_current_header()?;
        let id = self.database.get_current_id()?;
        let data = self.database.get_cell(id, &header)?;

        let result = TUI::get_editor_input(&data)?;
        self.database.update_cell(header.as_str(), id, &result)?;
        Ok(())
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        self.database.sort()
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

        database.move_cursor(Direction::Right, 256).unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let (_, res) = database.get(20, 100, "data".to_string()).unwrap();
        for row in res.iter() {
            let original = row[1].clone();
            let copy = row[4].clone();
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_long_test() {
        let p = Path::new("assets/data-long.csv");
        let mut database = Database::try_from(p).unwrap();

        let copy_fun = |s: String| Some(s.to_string());
        database.move_cursor(Direction::Right, 256).unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let table_name = database.get_current_table_name().unwrap();
        let (_, res) = database.get(20, 0, table_name).unwrap();
        for row in res.iter() {
            let original = row[1].clone();
            let copy = row[4].clone();
            assert_eq!(original, copy);
        }
    }
}
