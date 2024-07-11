use crate::app_error_other;
use crate::controller::command::{Command, PreviousCommand};
use crate::controller::direction::Direction;
use crate::controller::input::{self, InputMode};
use crate::error::{AppError, AppResult};
use crate::model::database::Database;
use crate::model::datarow::DataTable;
use crate::tui::TUI;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use log::info;
use std::path::PathBuf;

use super::command::QueuedCommand;
use super::input::StateMachine;

#[derive(Debug)]
pub struct Controller {
    pub(crate) ui: TUI,
    pub(crate) database: Database,
    pub(crate) last_command: PreviousCommand,
    queued_command: Option<QueuedCommand>,
    pub(crate) input_mode_state_machine: StateMachine,
}

impl Controller {
    pub fn new(ui: TUI, database: Database) -> Self {
        Self {
            ui,
            database,
            last_command: PreviousCommand::new(Command::None, None),
            queued_command: None,
            input_mode_state_machine: StateMachine::new(),
        }
    }

    pub(crate) fn save_to_sqlite_file(&self) -> AppResult<()> {
        if let Some(queued_command) = &self.queued_command {
            let filename = queued_command.inputs.first();
            if let Some(filename) = filename {
                let path = PathBuf::from(filename);
                Ok(self.database.backup_db(path)?)
            } else {
                Err(app_error_other!("No filename provided"))
            }
        } else {
            Err(app_error_other!("No queued command"))
        }
    }

    pub(crate) fn sql_query(&self) -> Result<(), AppError> {
        let query = self.queued_command.inputs.first().unwrap();
        self.database.sql_query(query)
    }

    fn enter_char(&mut self, new_char: char) {
        let index = self.byte_index();
        self.database.input.insert(index, new_char);
        self.move_cursor_right();
    }
    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.database.character_index.saturating_add(1);
        self.database.character_index = self.clamp_cursor(cursor_moved_right);
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.database.input.chars().count())
    }

    fn byte_index(&self) -> usize {
        self.database
            .input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.database.character_index)
            .unwrap_or(self.database.input.len())
    }

    fn submit_message(&mut self) {
        self.input_mode_state_machine
            .transition(input::Event::FinishEditing)
            .unwrap();
        self.queued_command.inputs.push(self.database.input.clone());
        self.reset_input();
    }

    fn reset_input(&mut self) {
        self.database.input.clear();
        self.reset_cursor();
    }

    fn reset_cursor(&mut self) {
        self.database.character_index = 0;
    }
    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.database.character_index.saturating_sub(1);
        self.database.character_index = self.clamp_cursor(cursor_moved_left);
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.database.character_index != 0;
        if is_not_cursor_leftmost {
            let current_index = self.database.character_index;
            let from_left_to_current_index = current_index - 1;
            let before_char_to_delete =
                self.database.input.chars().take(from_left_to_current_index);
            let after_char_to_delete = self.database.input.chars().skip(current_index);
            self.database.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn get_external_data(&mut self) -> AppResult<String> {
        TUI::get_external_editor_input(&self.database.input)
    }

    fn user_input_mode(&mut self) -> AppResult<()> {
        if let Event::Key(key) = event::read()? {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                if let KeyCode::Char('c') = key.code {
                    self.input_mode_state_machine
                        .transition(input::Event::AbortEditing)?;
                    self.reset_input();
                    return Ok(());
                }
            }
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Enter => self.submit_message(),
                    KeyCode::Char(to_insert) => {
                        self.enter_char(to_insert);
                    }
                    KeyCode::Backspace => {
                        self.delete_char();
                    }
                    KeyCode::Left => {
                        self.move_cursor_left();
                    }
                    KeyCode::Right => {
                        self.move_cursor_right();
                    }
                    KeyCode::Esc => {
                        self.input_mode_state_machine
                            .transition(input::Event::AbortEditing)?;
                        self.reset_input();
                    }
                    KeyCode::Tab => {
                        self.input_mode_state_machine
                            .transition(input::Event::UseExternalEditor)?;
                        let res = self.get_external_data();
                        if let Ok(data) = res {
                            self.database.character_index = data.len();
                            self.database.input = data;
                        }
                        self.input_mode_state_machine
                            .transition(input::Event::ExitExternalEditor)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn normal_mode(&mut self) -> AppResult<()> {
        if event::poll(std::time::Duration::from_millis(3000))? {
            let res = match if let Event::Key(key) = event::read()? {
                Ok(Command::from(key))
            } else {
                Err(app_error_other!("Could not poll"))
            } {
                Ok(command) => {
                    self.last_command = PreviousCommand::new(command.clone(), None);
                    let result = match command {
                        Command::RegexTransform
                        | Command::Save
                        | Command::RegexFilter
                        | Command::Edit
                        | Command::ExactSearch
                        | Command::SqlQuery
                        | Command::RenameColumn
                        | Command::RenameTable => {
                            self.queued_command = QueuedCommand::new(command.clone());
                            self.input_mode_state_machine
                                .transition(input::Event::StartEditing)?;
                            Ok(())
                        }
                        Command::Quit => Ok(()),
                        Command::Copy => self.copy(),
                        Command::IllegalOperation => Ok(()),
                        Command::None => Ok(()),
                        Command::Sort => self.sort(),
                        Command::Move(direction) => self.database.move_cursor(direction),
                        Command::NextTable => self.database.next_table(),
                        Command::PrevTable => self.database.prev_table(),
                        Command::TextToInt => self.text_to_int(),
                        Command::IntToText => self.int_to_text(),
                        Command::DeleteColumn => self.delete_column(),
                        Command::DeleteTable => self.database.delete_table(),
                        // Command::Join(_) => todo!(),
                    };
                    match command {
                        Command::RenameTable => {
                            let old_table_name = self.database.get_current_table_name()?;
                            self.database.character_index = old_table_name.len();
                            self.database.input = old_table_name;
                        }

                        Command::RenameColumn => {
                            log::info!("testing 2");
                        }
                        _ => {}
                    }

                    if command.requires_updating_view() {
                        self.database.slice.has_changed();
                    }
                    result
                }
                Err(result) => {
                    self.database.slice.has_changed();

                    self.last_command =
                        PreviousCommand::new(Command::IllegalOperation, Some(result.to_string()));
                    Ok(())
                }
            };
            if let Err(e) = res {
                self.database.slice.has_changed();
                self.last_command =
                    PreviousCommand::new(Command::IllegalOperation, Some(format!(": {}", e)));
            }
        }
        Ok(())
    }
    pub fn run(&mut self) -> AppResult<()> {
        loop {
            TUI::draw(self)?;
            // log::info!("state: {:?}", self.input_mode_state_machine.get_state());
            // log::info!("queued_command: {:?}", self.queued_command);
            match self.input_mode_state_machine.get_state() {
                InputMode::Normal if (self.queued_command.command == Command::None) => {
                    let res = self.normal_mode();
                    if self.last_command.command == Command::Quit {
                        self.ui.shutdown()?;
                        break Ok(());
                    }
                }
                InputMode::Normal => {
                    self.execute_queued_command()?;
                    self.queued_command = PreviousCommand::new(Command::None, None);
                }
                InputMode::Editing => {
                    let res = self.user_input_mode();
                }
                InputMode::Abort => {
                    self.last_command =
                        PreviousCommand::new(Command::None, Some("Aborted input".to_string()));

                    self.queued_command = PreviousCommand::new(Command::None, None);
                    self.input_mode_state_machine
                        .transition(input::Event::Reset)?;
                }
                InputMode::Finish => {
                    self.input_mode_state_machine
                        .transition(input::Event::Reset)?;
                }
                InputMode::ExternalEditor => todo!(),
            }
        }
    }
    pub fn get_headers_and_rows(&mut self, limit: u32) -> AppResult<DataTable> {
        let binding = "default table name".to_string();
        let first_table = self.database.get_current_table_name()?;
        self.database.get(limit, 0, first_table)
    }

    pub fn regex_filter(&mut self) -> AppResult<()> {
        let pattern = self.queued_command.message.clone().unwrap();
        let header = self.database.get_current_header()?;
        self.database.regex_filter(&header, &pattern)?;
        Ok(())
    }

    /// The user enters a regex and we will derive a new column from that.
    /// If the user enter a capture group we will ask for a second input that shows how the capture group should be transformed.
    /// If the user doesn't enter a capture group we will just copy the regex match.
    pub fn regex_transform(&mut self) -> AppResult<()> {
        let pattern = self.queued_command.message.clone().unwrap();
        let regex = regex::Regex::new(&pattern)?;
        let contains_capture_pattern = regex.capture_names().len() > 1;
        let header = self.database.get_current_header()?;
        if contains_capture_pattern {
            todo!();
            // let transformation = if cfg!(debug_assertions) {
            //     TUI::get_user_input(r"${1}${2}")?
            // } else {
            //     TUI::get_user_input(
            //         r"Enter transformation, e.g. '${first} ${second}' or '${1} ${2}' if un-named",
            //     )?
            // };
            // info!("pattern: {pattern:?}, transformation: {transformation:?}");

            // self.database
            //     .regex_capture_group_transform(&pattern, &header, &transformation)?;
        } else {
            info!("pattern: {pattern:?}");
            self.database
                .regex_no_capture_group_transform(&pattern, &header)?;
        }

        Ok(())
    }

    pub fn copy(&mut self) -> AppResult<()> {
        self.database.copy()?;
        self.last_command = PreviousCommand::new(Command::Copy, None);
        Ok(())
    }

    pub(crate) fn edit_cell(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let header = self.database.get_current_header()?;
        let id = self.database.get_current_id()?;
        let data = self.database.get_cell(id, &header)?;

        self.database
            .update_cell(header.as_str(), id, &edit_result)?;
        Ok(())
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        self.database.sort()
    }
    fn exact_search(&mut self) -> AppResult<()> {
        let pattern = self.queued_command.message.clone().unwrap();
        info!("pattern: {:?}", pattern);
        let header = self.database.get_current_header()?;
        match self.database.exact_search(&header, &pattern) {
            Ok(_) => {
                self.last_command =
                    PreviousCommand::new(Command::ExactSearch, Some("Match found".to_string()));
            }
            Err(_) => {
                self.last_command =
                    PreviousCommand::new(Command::ExactSearch, Some("No match found".to_string()));
            }
        }
        Ok(())
    }

    fn text_to_int(&mut self) -> AppResult<()> {
        self.last_command = PreviousCommand::new(Command::TextToInt, None);
        self.database.text_to_int()
    }

    fn int_to_text(&mut self) -> AppResult<()> {
        self.last_command = PreviousCommand::new(Command::IntToText, None);
        self.database.int_to_text()
    }

    fn delete_column(&mut self) -> Result<(), AppError> {
        let text = self.database.delete_column()?;
        self.last_command = PreviousCommand::new(Command::DeleteColumn, text);
        Ok(())
    }

    fn rename_column(&mut self) -> Result<(), AppError> {
        let new_column = self.queued_command.message.clone().unwrap();
        self.database.rename_column(&new_column)?;
        self.last_command = PreviousCommand::new(Command::RenameColumn, None);
        Ok(())
    }

    fn execute_queued_command(&mut self) -> AppResult<()> {
        if let Some(queued_command) = &self.queued_command {
            let inputs = queued_command.inputs;
            let result: AppResult<()> = match queued_command.command {
                Command::RegexTransform => {
                    self.regex_transform()?;
                    Ok(())
                }
                Command::Edit => self.edit_cell(inputs),
                // CREATE TABLE data2 AS SELECT firstname FROM data WHERE lastname = 'zenkert';
                Command::SqlQuery => self.sql_query(),
                Command::RegexFilter => self.regex_filter(),
                Command::RenameColumn => self.rename_column(),
                Command::RenameTable => self.rename_table(),
                Command::ExactSearch => self.exact_search(),
                _ => {
                    log::error!("Command not implemented: {:?}", queued_command.command);
                    return Err(AppError::from("Command not implemented"));
                } // Command::Quit => Ok(()),
                  // Command::Copy => self.copy(),
                  // Command::Edit => self.edit_cell(),
                  // Command::SqlQuery => self.sql_query(),
                  // Command::IllegalOperation => Ok(()),
                  // Command::None => Ok(()),
                  // Command::Sort => self.sort(),
                  // Command::Save => self.save_to_sqlite_file(),
                  // Command::Move(direction) => self.database.move_cursor(direction),
                  // Command::RegexFilter => self.regex_filter(),
                  // Command::NextTable => self.database.next_table(),
                  // Command::PrevTable => self.database.prev_table(),
                  // Command::ExactSearch => self.exact_search(),

                  // Command::TextToInt => self.text_to_int(),
                  // Command::IntToText => self.int_to_text(),
                  // Command::DeleteColumn => self.delete_column(),
                  // Command::RenameColumn => self.rename_column(),
                  // Command::Join(_) => todo!(),
            };
            if self.queued_command.command.requires_updating_view() {
                self.database.slice.has_changed();
            }
        }
        Ok(())
    }

    fn rename_table(&mut self) -> Result<(), AppError> {
        let new_table_name = self.queued_command.message.clone().unwrap();
        self.database.rename_table(&new_table_name)?;
        self.last_command = PreviousCommand::new(Command::RenameColumn, None);
        Ok(())
    }
    // Other methods from the Controller struct
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn copy_column_test() {
        let p = vec![PathBuf::from("assets/data.csv")];
        let mut database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| Some(s.to_string());

        database.move_cursor(Direction::Right).unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let (_, res) = database.get(20, 100, "data".to_string()).unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_long_test() {
        let p = vec![PathBuf::from("assets/data-long.csv")];
        let mut database = Database::try_from(p).unwrap();

        let copy_fun = |s: String| Some(s.to_string());
        database.move_cursor(Direction::Right).unwrap();
        let column_name = database.get_current_header().unwrap();
        database.derive_column(column_name, copy_fun).unwrap();
        let table_name = database.get_current_table_name().unwrap();
        let (_, res) = database.get(20, 0, table_name).unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }
}
