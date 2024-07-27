#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Editing,
    Abort,
    Finish,
    ExternalEditor,
}

#[derive(Debug)]
pub(crate) enum Event {
    StartEditing,
    AbortEditing,
    FinishEditing,
    UseExternalEditor,
    ExitExternalEditor,
    Reset,
}
#[derive(Debug)]
pub(crate) struct StateMachine {
    state: InputMode,
}

impl StateMachine {
    pub fn new() -> Self {
        StateMachine {
            state: InputMode::Normal,
        }
    }

    pub(crate) fn transition(&mut self, event: Event) -> Result<(), &str> {
        self.state = match (&self.state, event) {
            (InputMode::Normal, Event::StartEditing) => InputMode::Editing,
            (InputMode::Editing, Event::AbortEditing) => InputMode::Abort,
            (InputMode::Editing, Event::FinishEditing) => InputMode::Finish,
            (InputMode::Editing, Event::UseExternalEditor) => InputMode::ExternalEditor,
            (InputMode::ExternalEditor, Event::ExitExternalEditor) => InputMode::Editing,
            (InputMode::Abort | InputMode::Finish, Event::Reset) => InputMode::Normal,
            (state, _) => return Err("Invalid state change"), // No state change
        };
        Ok(())
    }
    // get state
    pub fn get_state(&self) -> InputMode {
        self.state
    }
}

// tests
#[cfg(test)]
mod tests {
    use crate::controller::input::*;

    #[test]
    fn test_state_machine() {
        let mut state_machine = StateMachine::new();
        assert_eq!(state_machine.state, InputMode::Normal);

        state_machine.transition(Event::StartEditing).unwrap();
        assert_eq!(state_machine.state, InputMode::Editing);

        state_machine.transition(Event::AbortEditing).unwrap();
        assert_eq!(state_machine.state, InputMode::Abort);

        // should error
        let result = state_machine.transition(Event::FinishEditing);
        assert!(result.is_err());
        assert_eq!(state_machine.state, InputMode::Abort);

        state_machine.transition(Event::Reset).unwrap();
        assert_eq!(state_machine.state, InputMode::Normal);

        state_machine.transition(Event::StartEditing).unwrap();
        assert_eq!(state_machine.state, InputMode::Editing);

        state_machine.transition(Event::UseExternalEditor).unwrap();
        assert_eq!(state_machine.state, InputMode::ExternalEditor);

        let result = state_machine.transition(Event::AbortEditing);
        assert!(result.is_err());
        assert_eq!(state_machine.state, InputMode::ExternalEditor);

        state_machine.transition(Event::ExitExternalEditor).unwrap();
        assert_eq!(state_machine.state, InputMode::Editing);

        state_machine.transition(Event::FinishEditing).unwrap();
        assert_eq!(state_machine.state, InputMode::Finish);
    }
}
