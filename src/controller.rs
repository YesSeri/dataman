use std::{
    io::{self, Error},
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
        self.database.get(limit, first_table)
    }
    //    pub fn run(&mut self) -> Result<(), AppError> {
    //        self.ui.update(&self.sheet)?;
    //        loop {
    //            // we don't need to poll I think, since we don't mind blocking.
    //            //if poll(Duration::from_secs(1))? {
    //            // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
    //            if let Event::Key(key) = read()? {
    //                match key.code {
    //                    KeyCode::Left => self.left(),
    //                    KeyCode::Right => self.right(),
    //                    KeyCode::Char(c) => {
    //                        if 'r' == c {
    //                            let (search, replace) = self.get_regex_input()?;
    //                            self.regex_command(Regex::new(&search)?, &replace)?;
    //                        }
    //                    }
    //                    _ => break,
    //                }
    //            }
    //            self.ui.update(&self.sheet)?;
    //            //}
    //        }
    //        self.ui.shutdown()?;
    //        Ok(())
    //    }
    fn get_regex_input(&mut self) -> Result<(String, String), AppError> {
        // self.sheet.change_mode(Mode::Regex);
        // 'outer: loop {
        //     self.ui.update_input(&self.sheet)?;
        //     let state = self.poll_for_input();
        //     match state {
        //         InputState::More => {
        //             continue;
        //         }
        //         InputState::Next => {
        //             let search = self.sheet.user_input.clone();
        //             self.sheet.change_mode(Mode::RegexReplace);
        //             loop {
        //                 self.ui.update_input(&self.sheet)?;
        //                 let state = self.poll_for_input();
        //                 match state {
        //                     InputState::More => {
        //                         continue;
        //                     }
        //                     nputState::Next => {
        //                         let replace = self.sheet.user_input.clone();
        //                         self.sheet.change_mode(Mode::Normal);
        //                         return Ok((search, replace));
        //                     }
        //                     InputState::Back => {
        //                         self.sheet.user_input = search;
        //                         self.sheet.change_mode(Mode::Regex);
        //                         continue 'outer;
        //                     }
        //                 }
        //             }
        //         }
        //         InputState::Back => {}
        //     }
        // }
        todo!();
    }

    fn regex_command(&mut self, find: Regex, replace: &str) -> Result<(), AppError> {
        // let fun = |s: String| {
        //     let result: String = match find.captures_len() {
        //         1 => find.replace_all(&s, replace).to_string(),
        //         _ => todo!(),
        //     };
        //     result
        // };
        // self.sheet.derive_new(fun);
        // self.sheet.change_mode(Mode::Normal);
        Ok(())
    }
    fn poll_for_input(&mut self) -> InputState {
        // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
        if let Ok(Event::Key(key)) = read() {
            match key.code {
                KeyCode::Enter => InputState::Next,
                // KeyCode::Esc => InputState::Back,
                // KeyCode::Backspace => {
                //     self.sheet.user_input.pop();
                //     return InputState::More;
                // }
                // KeyCode::Char(c) => {
                //     self.sheet.user_input.push(c);
                //     return InputState::More;
                // }
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
        let (_, res) = database.get(20, "data").unwrap();
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
        let (_, res) = database.get(20, table_name).unwrap();
        for row in res.iter() {
            let original = row[1].clone();
            let copy = row[4].clone();
            assert_eq!(original, copy);
        }
    }
}
