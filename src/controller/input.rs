use super::command::{Command, PreviousCommand};

#[derive(Debug, Clone)]
pub(crate) struct EditingSession {
    pub(crate) command: Command,
    pub(crate) inputs: Vec<String>,
    pub(crate) text: String,
    pub(crate) cursor: usize,
}
impl EditingSession {
    pub(crate) fn new(command: Command, text: String) -> Self {
        Self {
            command,
            inputs: vec![],
            cursor: text.chars().count(),
            text,
        }
    }
    pub(crate) fn prompt(&self) -> String {
        if self.command == Command::RegexTransform && !self.inputs.is_empty() {
            "Capture template (e.g. ${1} ${2})".into()
        } else {
            self.command.prompt().into()
        }
    }
}

/// Interaction state has one owner: the frontend. Worker snapshots never carry it.
#[derive(Debug, Clone)]
pub(crate) struct UiState {
    pub(crate) editing: Option<EditingSession>,
    pub(crate) last_command: PreviousCommand,
    pub(crate) history_view: bool,
    pub(crate) history_cursor: usize,
}
impl Default for UiState {
    fn default() -> Self {
        Self {
            editing: None,
            last_command: PreviousCommand::new(Command::None, None),
            history_view: false,
            history_cursor: 0,
        }
    }
}

use crate::{
    error::{AppError, AppResult},
    tui::TUI,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
pub(crate) fn edit_prompt(
    ui: &mut UiState,
    key: KeyEvent,
) -> AppResult<Option<(Command, Vec<String>)>> {
    let Some(session) = ui.editing.as_mut() else {
        return Ok(None);
    };
    let byte = session
        .text
        .char_indices()
        .nth(session.cursor)
        .map(|(i, _)| i)
        .unwrap_or(session.text.len());
    match key.code {
        KeyCode::Esc | KeyCode::Char('c')
            if key.code == KeyCode::Esc || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            ui.editing = None;
        }
        KeyCode::Char(character) => {
            session.text.insert(byte, character);
            session.cursor += 1;
        }
        KeyCode::Backspace if session.cursor > 0 => {
            let previous = session
                .text
                .char_indices()
                .nth(session.cursor - 1)
                .map(|(index, _)| index)
                .ok_or_else(|| AppError::invalid_input("Input cursor is out of bounds"))?;
            session.text.replace_range(previous..byte, "");
            session.cursor -= 1;
        }
        KeyCode::Left => session.cursor = session.cursor.saturating_sub(1),
        KeyCode::Right => session.cursor = (session.cursor + 1).min(session.text.chars().count()),
        KeyCode::Tab => match TUI::get_external_editor_input(&session.text) {
            Ok(text) => {
                session.cursor = text.chars().count();
                session.text = text;
            }
            Err(error) => ui.last_command.message = Some(error.present()),
        },
        KeyCode::Enter => {
            let template = session.command == Command::RegexTransform
                && session.inputs.is_empty()
                && regex::Regex::new(&session.text).is_ok_and(|regex| regex.captures_len() > 1);
            session.inputs.push(std::mem::take(&mut session.text));
            session.cursor = 0;
            if !template {
                let session = ui
                    .editing
                    .take()
                    .ok_or_else(|| AppError::from("Missing command"))?;
                return Ok(Some((session.command, session.inputs)));
            }
        }
        _ => {}
    }
    Ok(None)
}
