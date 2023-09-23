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
use regex::Regex;

use crate::{
    error::AppError,
    tui::TUI,
    view::{BasicUI, Display},
};
use crate::libstuff::model::{Mode, Sheet};

#[derive(Debug)]
pub struct Controller<T> {
    ui: T,
    sheet: Sheet,
}

// impl TryFrom<PathBuf> for Controller<BasicUI> {
//     type Error = AppError;
//
//     fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
//         Ok(Self {
//             sheet: Sheet::try_from(path)?,
//             ui: BasicUI::new(),
//         })
//     }
// }
// impl TryFrom<PathBuf> for Controller<TUI> {
//     type Error = AppError;
//
//     fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
//         Ok(Self {
//             sheet: Sheet::try_from(path)?,
//             ui: TUI::new(),
//         })
//     }
// }

impl<T: Display> Controller<T> {
    pub fn new(ui: T, sheet: Sheet) -> Self {
        Self { ui, sheet }
    }

    pub fn run(&mut self) -> Result<(), AppError> {
        self.ui.update(&self.sheet)?;
        self.ui.shutdown()?;
        Ok(())
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
        self.sheet.change_mode(Mode::Regex);
        'outer: loop {
            self.ui.update_input(&self.sheet)?;
            let state = self.poll_for_input();
            match state {
                InputState::More => {
                    continue;
                }
                InputState::Next => {
                    let search = self.sheet.user_input.clone();
                    self.sheet.change_mode(Mode::RegexReplace);
                    loop {
                        self.ui.update_input(&self.sheet)?;
                        let state = self.poll_for_input();
                        match state {
                            InputState::More => {
                                continue;
                            }
                            InputState::Next => {
                                let replace = self.sheet.user_input.clone();
                                self.sheet.change_mode(Mode::Normal);
                                return Ok((search, replace));
                            }
                            InputState::Back => {
                                self.sheet.user_input = search;
                                self.sheet.change_mode(Mode::Regex);
                                continue 'outer;
                            }
                        }
                    }
                }
                InputState::Back => {}
            }
        }
    }

    fn regex_command(&mut self, find: Regex, replace: &str) -> Result<(), AppError> {
        let fun = |s: String| {
            let result: String = match find.captures_len() {
                1 => find.replace_all(&s, replace).to_string(),
                _ => todo!(),
            };
            result
        };
        self.sheet.derive_new(fun);
        self.sheet.change_mode(Mode::Normal);
        Ok(())
    }
    fn poll_for_input(&mut self) -> InputState {
        // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
        if let Ok(Event::Key(key)) = read() {
            match key.code {
                KeyCode::Enter => InputState::Next,
                KeyCode::Esc => InputState::Back,
                KeyCode::Backspace => {
                    self.sheet.user_input.pop();
                    return InputState::More;
                }
                KeyCode::Char(c) => {
                    self.sheet.user_input.push(c);
                    return InputState::More;
                }
                _ => InputState::Back,
            }
        } else {
            InputState::More
        }
    }

    fn left(&mut self) {
        self.sheet.cursor = self.sheet.cursor.saturating_sub(1);
    }
    fn right(&mut self) {
        if self.sheet.cursor < self.sheet.columns.len() - 1 {
            self.sheet.cursor += 1;
        }
    }
}
enum InputState {
    More,
    Next,
    Back,
}
#[cfg(test)]
mod test {
    use regex::Regex;

    use crate::view::DebugUI;

    use super::*;

    #[test]
    fn regex_derive_column() {
        let data = r"col1,col2
abc,def
";
        let sheet = Sheet::try_from(data).unwrap();
        let d: DebugUI = Display::new();
        let mut controller: Controller<_> = Controller::new(d, sheet);
        let re = Regex::new("[a|e]").unwrap();
        controller.regex_command(re, "x").unwrap();
        let sheet = &controller.sheet;
        assert_eq!(sheet.columns.len(), 3);
        assert_eq!(sheet.columns[2].get_data()[0], "xbc");
    }
}
