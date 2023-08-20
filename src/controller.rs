use std::{error::Error, io, path::PathBuf, thread::sleep, time::Duration};

use crossterm::{terminal, ExecutableCommand};
use regex::Regex;

use crate::{
    model::{Mode, Sheet},
    view::Display,
};
use crossterm::{
    cursor::position,
    event::{poll, read, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};

pub struct Controller<T> {
    ui: T,
    sheet: Sheet,
}

impl Controller<()> {
    fn execute_regex(sheet: &mut Sheet, re: Regex) -> Result<(), std::io::Error> {
        let fun = |s: String| {
            let result: String = match re.captures_len() {
                1 => re.replace_all(&s, "").to_string(),
                _ => todo!(),
            };
            result
        };
        sheet.derive_new(fun);
        Ok(())
    }
}

impl<T: Display> Controller<T> {
    pub fn new(ui: T, sheet: Sheet) -> Self {
        Self { ui, sheet }
    }
    pub fn from(pathbuf: &PathBuf) -> Self {
        Self {
            ui: Display::new(),
            sheet: Sheet::from_csv(pathbuf).unwrap(),
        }
    }
    pub fn run(&mut self) -> Result<(), std::io::Error> {
        self.ui.update(&self.sheet)?;
        loop {
            if poll(Duration::from_secs(1))? {
                // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
                if let Event::Key(key) = read()? {
                    match key.code {
                        KeyCode::Left => self.left(),
                        KeyCode::Right => self.right(),
                        KeyCode::Char(c) => match c {
                            'r' => {
                                self.sheet.change_mode(Mode::Regex);
                                if let Ok(_) = self.get_user_input() {
                                    // Execute regex input
                                    let regex =
                                        Regex::new(self.sheet.user_inputs.last().unwrap()).unwrap();
                                    Controller::execute_regex(&mut self.sheet, regex)?;
                                }
                                self.sheet.change_mode(Mode::Normal);
                            }
                            _ => break,
                        },
                        _ => break,
                    }
                }
                self.ui.update(&self.sheet)?;
            }
        }
        self.ui.shutdown()?;
        Ok(())
    }

    fn get_user_input(&mut self) -> Result<(), std::io::Error> {
        self.ui.update(&self.sheet)?;
        loop {
            if poll(Duration::from_secs(1))? {
                // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
                if let Event::Key(key) = read()? {
                    let current_input = &mut self.sheet.user_inputs.last_mut().unwrap();
                    match key.code {
                        KeyCode::Enter => break,
                        KeyCode::Esc => break,
                        KeyCode::Backspace => {
                            current_input.pop();
                        }
                        KeyCode::Char(c) => current_input.push(c),
                        _ => break,
                    }
                }
                self.ui.update(&self.sheet)?;
            }
        }

        Ok(())
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
#[cfg(test)]
mod test {
    use regex::Regex;

    use crate::view::{BasicUI, DebugUI};

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
        Controller::execute_regex(&mut controller.sheet, re).unwrap();
        let sheet = &controller.sheet;
        assert_eq!(sheet.columns.len(), 3);
        assert_eq!(sheet.columns[2].get_data()[0], "bc");
    }
}
