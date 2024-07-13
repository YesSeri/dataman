use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

use super::command::Command;

pub enum Mode {
    Normal,
    Insert,
}

pub struct CommandParser {
    mode: Mode,
    key_buffer: Vec<KeyEvent>,
    keybindings: HashMap<Vec<KeyEvent>, Command>,
    numeric_prefix: Option<u32>,
}

impl CommandParser {
    pub fn new() -> Self {
        let mut keybindings = HashMap::new();

        // Define keybindings for sequences with numeric prefix support
        keybindings.insert(
            vec![
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            ],
            Command::DeleteColumn,
        );

        keybindings.insert(
            vec![
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            ],
            Command::RegexTransform,
        );

        CommandParser {
            mode: Mode::Normal,
            key_buffer: Vec::new(),
            keybindings,
            numeric_prefix: None,
        }
    }

    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> bool {
        match self.mode {
            Mode::Normal => match key_event.code {
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    self.numeric_prefix =
                        Some(self.numeric_prefix.unwrap_or(0) * 10 + c.to_digit(10).unwrap());
                    println!("Numeric prefix: {}", self.numeric_prefix.unwrap());
                }
                _ => {
                    self.key_buffer.push(key_event);
                    if let Some(action) = self.keybindings.get(&self.key_buffer) {
                        let count = self.numeric_prefix.unwrap_or(1);
                        match action {
                            Command::DeleteColumn => {
                                println!("Delete {} columns", count);
                            }
                            Command::RegexTransform => {
                                println!("Regex transform {} times", count);
                            }
                            _ => todo!(),
                        }
                        self.key_buffer.clear();
                        self.numeric_prefix = None;
                    } else if self.key_buffer.len() > 2 {
                        self.key_buffer.clear();
                        self.numeric_prefix = None;
                    }
                    if key_event.code == KeyCode::Char('i') {
                        self.mode = Mode::Insert;
                        println!("Switch to Insert Mode");
                    } else if key_event.code == KeyCode::Char('q') {
                        return true;
                    }
                }
            },
            Mode::Insert => {
                if key_event.code == KeyCode::Esc {
                    self.mode = Mode::Normal;
                    println!("Switch to Normal Mode");
                } else {
                    println!("Insert Mode: {:?}", key_event.code);
                }
            }
        }
        false
    }
}
