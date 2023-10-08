use std::{
    io::{self, Error, Read, Write},
    path::PathBuf,
    thread::sleep,
    time::Duration,
};

use crossterm::{
    event::{poll, read, Event, KeyCode, KeyEvent},
    terminal, ExecutableCommand,
};
use ratatui::widgets::TableState;
use regex::Regex;

use crate::{
    error::AppError,
    tui::{Command, TUI},
};
use crate::{error::AppResult, libstuff::db::Database};

pub struct Controller {
    pub ui: TUI,
    pub database: Database,
}

impl Controller {
    pub fn new(ui: TUI, database: Database) -> Self {
        Self { ui, database }
    }

    pub fn run(&mut self) -> Result<(), AppError> {
        TUI::start(self)?;
        self.ui.shutdown()
    }
    pub fn get_headers_and_rows(&self, limit: i32) -> AppResult<(Vec<String>, Vec<Vec<String>>)> {
        let binding = "default table name".to_string();
        let first_table = self.database.table_names.iter().next().unwrap_or(&binding);
        self.database.get(limit, 0, first_table)
    }
    fn poll_for_input(&mut self) -> InputState {
        // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
        if let Ok(Event::Key(key)) = read() {
            match key.code {
                KeyCode::Enter => InputState::Next,
                _ => InputState::Back,
            }
        } else {
            InputState::More
        }
    }

    pub fn regex(&self) {
        todo!();
    }

    pub fn derive_column(&self, fun: fn(String) -> String) -> AppResult<()> {
        let column_name = self.database.get_current_header()?;
        self.database.derive_column(column_name, fun)
    }

    pub fn copy(&mut self) {
        let fun = |s: String| s.to_string();
        self.ui.set_command(Command::Copy);
        let _ = self.derive_column(fun);
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
enum InputState {
    More,
    Next,
    Back,
}
#[cfg(test)]
mod test {
    use std::path::Path;

    use regex::Regex;

    use super::*;

    #[test]
    fn copy_column_test() {
        let p = Path::new("assets/data.csv");
        let mut database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| s.to_string();
        database.next_header().unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let (_, res) = database.get(20, 100, "data").unwrap();
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

        let copy_fun = |s: String| s.to_string();
        database.next_header().unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let table_name = database.table_names.iter().next().unwrap();
        let (_, res) = database.get(20, 0, table_name).unwrap();
        for row in res.iter() {
            let original = row[1].clone();
            let copy = row[4].clone();
            assert_eq!(original, copy);
        }
    }
}
