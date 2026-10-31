//! Stateful profile dispatcher, shared by the terminal and automation.
use crate::controller::Command;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

#[derive(Debug)]
pub struct Dispatcher {
    bindings: HashMap<String, String>,
    pending: String,
    contexts: HashMap<String, HashMap<String, String>>,
    sheet: String,
}
impl Dispatcher {
    pub fn new(profile: &str, overrides: bool) -> Self {
        let legacy: crate::KeyBindings =
            toml::from_str(include_str!("../../assets/keybindings-dataman.toml")).unwrap();
        let mut bindings = if profile == "dataman" {
            legacy.bindings
        } else {
            [
                ("q", "quit"),
                ("ctrl+c", "quit"),
                ("left", "move_left"),
                ("right", "move_right"),
                ("up", "move_up"),
                ("down", "move_down"),
                ("h", "move_left"),
                ("j", "move_down"),
                ("k", "move_up"),
                ("l", "move_right"),
                ("gt", "invert_selection"),
                ("gu", "clear_selection"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect()
        };
        let mut contexts = HashMap::new();
        if overrides {
            if let Ok(contents) = std::fs::read_to_string("keybindings.toml") {
                if let Ok(custom) = toml::from_str::<crate::KeyBindings>(&contents) {
                    bindings.extend(custom.bindings);
                    contexts = custom.contexts;
                }
            }
        }
        Self {
            bindings,
            pending: String::new(),
            contexts,
            sheet: String::new(),
        }
    }
    pub fn key(&mut self, key: KeyEvent, sheet: &str) -> Command {
        if self.sheet != sheet {
            self.pending.clear();
            self.sheet = sheet.into();
        }
        if key.code == KeyCode::Esc {
            self.pending.clear();
            return Command::HistoryClose;
        }
        let token = crate::controller::command::normalized_key(&key);
        self.pending.push_str(&token);
        let scoped = self.contexts.get(sheet);
        if let Some(name) = scoped
            .and_then(|b| b.get(&self.pending))
            .or_else(|| self.bindings.get(&self.pending))
        {
            let command = super::registry::resolve(name).unwrap_or(Command::None);
            self.pending.clear();
            return command;
        }
        if !self
            .bindings
            .keys()
            .chain(scoped.into_iter().flat_map(|b| b.keys()))
            .any(|k| k.starts_with(&self.pending))
        {
            self.pending.clear();
        }
        Command::None
    }
    pub fn bindings(&self) -> &HashMap<String, String> {
        &self.bindings
    }
    pub fn bind_context(&mut self, sheet: String, sequence: String, command: String) {
        self.contexts
            .entry(sheet)
            .or_default()
            .insert(sequence, command);
    }
    pub fn pending(&self) -> &str {
        &self.pending
    }
}
pub fn parse_key(token: &str) -> Result<KeyEvent, String> {
    let (mods, name) = if let Some(s) = token.strip_prefix("ctrl+") {
        (KeyModifiers::CONTROL, s)
    } else {
        (KeyModifiers::NONE, token)
    };
    let code = match name {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" => KeyCode::Esc,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "backspace" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        s if s.starts_with('f') && s[1..].parse::<u8>().is_ok() => {
            KeyCode::F(s[1..].parse().unwrap())
        }
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        _ => return Err(format!("unsupported key {token}")),
    };
    Ok(KeyEvent::new(code, mods))
}
