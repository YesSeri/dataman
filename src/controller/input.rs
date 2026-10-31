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
