use std::{error::Error, io, path::PathBuf, thread::sleep, time::Duration};

use crossterm::{terminal, ExecutableCommand};
use regex::Regex;

use crate::{model::{Sheet, Mode}, view::Display};
use crossterm::{
    cursor::position,
    event::{poll, read, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};

pub struct Controller<T: Display> {
    ui: T,
    sheet: Sheet,
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
                                self.get_user_input()?;
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
    fn left(&mut self) {
        self.sheet.cursor = self.sheet.cursor.saturating_sub(1);
    }
    fn right(&mut self) {
        if self.sheet.cursor < self.sheet.columns.len() - 1 {
            self.sheet.cursor += 1;
        }
    }

    fn get_user_input(&mut self) -> Result<(), std::io::Error> {
        self.sheet.change_mode(Mode::Regex);
        self.ui.update(&self.sheet)?;
        loop {
            if poll(Duration::from_secs(1))? {
                // It's guaranteed that read() won't block if `poll` returns `Ok(true)`
                if let Event::Key(ref mut key) = read()? {
                    let current_input = &mut self.sheet.user_inputs.last_mut().unwrap();
                    match key.code {
                        KeyCode::Enter => break,
                        KeyCode::Esc => break,
                        KeyCode::Backspace => {
                            current_input.pop();
                        },
                        KeyCode::Char(c) => current_input.push(c),
                        _ => break,
                    }
                }
                self.ui.update(&self.sheet)?;
            }
        }

        self.sheet.change_mode(Mode::Normal);
        Ok(())
    }

    pub fn debug_run(&mut self) {
        let mut stdout = io::stdout();
        stdout
            .execute(terminal::Clear(terminal::ClearType::All))
            .unwrap();
        self.ui.update(&self.sheet).unwrap();
        sleep(Duration::from_millis(1000));

        let user_regex = "e+";
        let user_replace_regex = "";
        let fun = |s: String| {
            let re = Self::create_regex(user_regex).expect("Expected valid regex.");
            let result: String = match re.captures_len() {
                1 => re.replace_all(&s, user_replace_regex).to_string(),
                _ => todo!(),
            };
            result
        };
        self.sheet.derive_new(1, fun);
        self.ui.update(&self.sheet).unwrap();
        sleep(Duration::from_millis(1000));
    }
    fn create_regex(regex_str: &str) -> Result<Regex, regex::Error> {
        Ok(Regex::new(regex_str)?)
    }
}
