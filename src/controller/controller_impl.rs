use crate::app_error_other;
use crate::controller::command::{Command, PreviousCommand};
use crate::controller::input::{self, InputMode};
use crate::error::{AppError, AppResult};
use crate::io::savers;
use crate::model::database::Database;
use crate::model::datarow::{DataItem, DataTable};
use crate::navigation::Direction;
use crate::tui::TUI;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::path::PathBuf;

use super::command::QueuedCommand;
use super::input::StateMachine;

const POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(3000);

#[derive(Debug)]
pub struct Controller {
    pub(crate) database: Database,
    pub(crate) ui: UiState,
    pub(crate) view: ViewState,
}

#[derive(Debug)]
pub(crate) struct ViewState {
    pub(crate) table_name: String,
    pub(crate) column: usize,
    pub(crate) row: usize,
    pub(crate) row_offset: u32,
    pub(crate) order_column: Option<String>,
    pub(crate) ascending: bool,
    pub(crate) headers: Vec<String>,
    pub(crate) rows: Vec<Vec<DataItem>>,
    pub(crate) table_before_metadata: Option<String>,
}

/// Terminal interaction state kept out of the SQLite-backed data model.
#[derive(Debug)]
pub(crate) struct UiState {
    pub(crate) input: String,
    pub(crate) character_index: usize,
    pub(crate) last_command: PreviousCommand,
    pub(crate) queued_command: Option<QueuedCommand>,
    pub(crate) input_mode_state_machine: StateMachine,
}

impl Controller {
    pub fn new(database: Database) -> Self {
        let table_name = database
            .get_table_names()
            .ok()
            .and_then(|names| names.into_iter().find(|n| n != "table_of_tables"))
            .unwrap_or_default();
        Self {
            database,
            view: ViewState {
                table_name,
                column: 0,
                row: 0,
                row_offset: 0,
                order_column: Some("id".into()),
                ascending: true,
                headers: vec![],
                rows: vec![],
                table_before_metadata: None,
            },
            ui: UiState {
                input: String::new(),
                character_index: 0,
                last_command: PreviousCommand::new(Command::None, None),
                queued_command: None,
                input_mode_state_machine: StateMachine::new(),
            },
        }
    }

    pub(crate) fn ordering(&self) -> String {
        match &self.view.order_column {
            Some(column) => format!(
                r#"ORDER BY "{}" {}"#,
                column,
                if self.view.ascending { "ASC" } else { "DESC" }
            ),
            None => String::new(),
        }
    }

    fn selected_column(&self) -> AppResult<String> {
        self.database
            .get_headers(&self.view.table_name)?
            .get(self.view.column)
            .cloned()
            .ok_or_else(|| app_error_other!("Could not get header"))
    }

    fn move_cursor(&mut self, direction: Direction, height: u32) -> AppResult<()> {
        match direction {
            Direction::Left => {
                let count = self.database.count_headers(&self.view.table_name)? as usize;
                if count > 0 {
                    self.view.column = (self.view.column + count - 1) % count;
                }
            }
            Direction::Right => {
                let count = self.database.count_headers(&self.view.table_name)? as usize;
                if count > 0 {
                    self.view.column = (self.view.column + 1) % count;
                }
            }
            Direction::Up => {
                if self.view.row == 0 && self.view.row_offset > 0 {
                    self.view.row_offset = self.view.row_offset.saturating_sub(height);
                    self.view.row = height.saturating_sub(1) as usize;
                } else {
                    self.view.row = self.view.row.saturating_sub(1);
                }
            }
            Direction::Down => {
                let count = self
                    .database
                    .count_rows(&self.view.table_name)
                    .unwrap_or(u32::MAX);
                if height > 0
                    && self.view.row + 1 >= height as usize
                    && self.view.row_offset + self.view.row as u32 + 1 < count
                {
                    self.view.row_offset = self.view.row_offset.saturating_add(height);
                    self.view.row = 0;
                } else if self.view.row_offset + self.view.row as u32 + 1 < count {
                    self.view.row += 1;
                }
            }
        }
        Ok(())
    }

    fn change_table(&mut self, next: bool) -> AppResult<()> {
        let mut names = self.database.get_table_names()?;
        names.retain(|name| name != "table_of_tables");
        let index = names
            .iter()
            .position(|name| name == &self.view.table_name)
            .unwrap_or(0);
        if !names.is_empty() {
            let target = if next {
                (index + 1) % names.len()
            } else {
                (index + names.len() - 1) % names.len()
            };
            self.view.table_name = names[target].clone();
            self.view.column = 0;
            self.view.row = 0;
            self.view.row_offset = 0;
        } else if self
            .database
            .get_table_names()?
            .iter()
            .any(|name| name == "table_of_tables")
        {
            self.view.table_name = "table_of_tables".into();
            self.view.column = 0;
            self.view.row = 0;
            self.view.row_offset = 0;
        }
        self.normalize_sort_column()?;
        Ok(())
    }

    fn normalize_sort_column(&mut self) -> AppResult<()> {
        let headers = self.database.get_headers(&self.view.table_name)?;
        if !self
            .view
            .order_column
            .as_ref()
            .is_some_and(|column| headers.contains(column))
        {
            self.view.order_column = headers.iter().find(|header| *header == "id").cloned();
            self.view.ascending = true;
        }
        Ok(())
    }

    fn delete_table(&mut self) -> AppResult<()> {
        let table = self.view.table_name.clone();
        self.database.delete_table(&table)?;
        self.change_table(false)
    }

    fn toggle_metadata_table(&mut self) -> AppResult<()> {
        if self.view.table_name == "table_of_tables" {
            let names = self.database.get_table_names()?;
            self.view.table_name = self
                .view
                .table_before_metadata
                .take()
                .filter(|name| names.contains(name))
                .or_else(|| names.into_iter().find(|name| name != "table_of_tables"))
                .unwrap_or_else(|| "table_of_tables".into());
            self.view.column = 0;
            self.view.row = 0;
            self.view.row_offset = 0;
            return Ok(());
        }
        self.database.view_metadata_table()?;
        self.view.table_before_metadata = Some(self.view.table_name.clone());
        self.view.table_name = "table_of_tables".into();
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        Ok(())
    }

    pub(crate) fn save_to_sqlite_file(&self) -> AppResult<()> {
        if let Some(queued_command) = &self.ui.queued_command {
            let filename = queued_command.inputs.first();
            if let Some(filename) = filename {
                let path = PathBuf::from(filename);
                Ok(savers::sqlite(&self.database, &path)?)
            } else {
                Err(app_error_other!("No filename provided"))
            }
        } else {
            Err(app_error_other!("No queued command"))
        }
    }

    pub(crate) fn sql_query(&self, inputs: Vec<String>) -> Result<(), AppError> {
        let query = inputs[0].to_owned();
        self.database.sql_query(&query)
    }

    fn enter_char(&mut self, new_char: char) {
        let index = self.byte_index();
        self.ui.input.insert(index, new_char);
        self.move_cursor_right();
    }
    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.ui.character_index.saturating_add(1);
        self.ui.character_index = self.clamp_cursor(cursor_moved_right);
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.ui.input.chars().count())
    }

    fn byte_index(&self) -> usize {
        self.ui
            .input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.ui.character_index)
            .unwrap_or(self.ui.input.len())
    }

    fn submit_message(&mut self) {
        if let Some(queued_command) = &mut self.ui.queued_command {
            if queued_command.command != Command::RegexTransform || queued_command.inputs.len() == 1
            {
                self.ui
                    .input_mode_state_machine
                    .transition(input::Event::FinishEditing)
                    .unwrap();
            }
            queued_command.inputs.push(self.ui.input.clone());
        }
        self.reset_input();
    }

    fn reset_input(&mut self) {
        self.ui.input.clear();
        self.reset_cursor();
    }

    fn reset_cursor(&mut self) {
        self.ui.character_index = 0;
    }
    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.ui.character_index.saturating_sub(1);
        self.ui.character_index = self.clamp_cursor(cursor_moved_left);
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.ui.character_index != 0;
        if is_not_cursor_leftmost {
            let current_index = self.ui.character_index;
            let from_left_to_current_index = current_index - 1;
            let before_char_to_delete = self.ui.input.chars().take(from_left_to_current_index);
            let after_char_to_delete = self.ui.input.chars().skip(current_index);
            self.ui.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn get_external_data(&mut self) -> AppResult<String> {
        TUI::get_external_editor_input(&self.ui.input)
    }

    fn user_input_mode(&mut self) -> AppResult<()> {
        if let Event::Key(key) = event::read()? {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                if let KeyCode::Char('c') = key.code {
                    self.ui
                        .input_mode_state_machine
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
                        self.ui
                            .input_mode_state_machine
                            .transition(input::Event::AbortEditing)?;
                        self.reset_input();
                    }
                    KeyCode::Tab => {
                        self.ui
                            .input_mode_state_machine
                            .transition(input::Event::UseExternalEditor)?;
                        let res = self.get_external_data();
                        if let Ok(data) = res {
                            self.ui.character_index = data.len();
                            self.ui.input = data;
                        }
                        self.ui
                            .input_mode_state_machine
                            .transition(input::Event::ExitExternalEditor)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn normal_mode(&mut self) -> AppResult<()> {
        if event::poll(POLL_TIMEOUT)? {
            let res = match if let Event::Key(key) = event::read()? {
                Ok(Command::from(key))
            } else {
                Err(app_error_other!("Could not poll"))
            } {
                Ok(command) => {
                    self.ui.last_command = PreviousCommand::new(command.clone(), None);
                    let result = match command {
                        Command::RegexTransform
                        | Command::Save
                        | Command::RegexFilter
                        | Command::Edit
                        | Command::ExactSearch
                        | Command::SqlQuery
                        | Command::RenameColumn
                        | Command::MathOperation
                        | Command::RenameTable => {
                            self.ui.queued_command = Some(QueuedCommand::new(command.clone()));
                            self.ui
                                .input_mode_state_machine
                                .transition(input::Event::StartEditing)?;
                            Ok(())
                        }
                        Command::Quit => Ok(()),
                        Command::Copy => self.copy(),
                        Command::IllegalOperation => Ok(()),
                        Command::None => Ok(()),
                        Command::Sort => self.sort(),
                        Command::Move(direction) => {
                            self.move_cursor(direction, TUI::get_table_height()?)
                        }
                        Command::NextTable => self.change_table(true),
                        Command::PrevTable => self.change_table(false),
                        Command::TextToInt => self.text_to_int(),
                        Command::IntToText => self.int_to_text(),
                        Command::DeleteColumn => self.delete_column(),
                        Command::DeleteTable => self.delete_table(),
                        Command::ToggleMetadataTable => self.toggle_metadata_table(),
                        // Command::Join(_) => todo!(),
                    };
                    match command {
                        Command::RenameTable => {
                            let old_table_name = self.view.table_name.clone();
                            self.ui.character_index = old_table_name.len();
                            self.ui.input = old_table_name;
                        }

                        Command::RenameColumn => {
                            log::info!("testing 2");
                        }
                        _ => {}
                    }

                    if let Err(err) = &result {
                        log::info!("Error: {:?}", err);
                    }
                    result
                }
                Err(result) => {
                    self.ui.last_command =
                        PreviousCommand::new(Command::IllegalOperation, Some(result.to_string()));
                    Ok(())
                }
            };
            if let Err(e) = res {
                self.ui.last_command =
                    PreviousCommand::new(Command::IllegalOperation, Some(format!(": {}", e)));
            }
        }
        Ok(())
    }
    pub fn run(&mut self, mut tui: TUI) -> AppResult<()> {
        loop {
            tui.draw(self)?;
            match self.ui.input_mode_state_machine.get_state() {
                InputMode::Finish => {
                    if let Some(queued_command) = self.ui.queued_command.clone() {
                        self.execute_queued_command()?;
                        self.ui.queued_command = None;
                        self.ui.last_command = PreviousCommand::new(queued_command.command, None);
                        self.ui
                            .input_mode_state_machine
                            .transition(input::Event::Reset)?;
                    } else {
                        return Err(app_error_other!("No queued command, but in finish state"));
                    }
                }
                InputMode::Editing if (self.ui.queued_command.is_some()) => {
                    let res = self.user_input_mode();
                }
                InputMode::Normal => {
                    let res = self.normal_mode();
                    if self.ui.last_command.command == Command::Quit {
                        tui.shutdown()?;
                        break Ok(());
                    }
                }
                InputMode::Abort => {
                    self.ui.last_command =
                        PreviousCommand::new(Command::None, Some("Aborted input".to_string()));

                    self.ui.queued_command = None;
                    self.ui
                        .input_mode_state_machine
                        .transition(input::Event::Reset)?;
                }
                InputMode::ExternalEditor => todo!(),
                InputMode::Editing => unreachable!(),
            }
        }
    }
    pub fn get_headers_and_rows(&mut self, limit: u32) -> AppResult<DataTable> {
        let ordering = self.ordering();
        let page = self.database.get(
            limit,
            self.view.row_offset,
            &self.view.table_name,
            &ordering,
        )?;
        self.view.headers.clone_from(&page.0);
        self.view.rows.clone_from(&page.1);
        self.view.row = self.view.row.min(self.view.rows.len().saturating_sub(1));
        Ok(page)
    }

    pub fn regex_filter(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let pattern = inputs[0].to_owned();
        let header = self.selected_column()?;
        self.view.table_name =
            self.database
                .regex_filter(&self.view.table_name, &header, &pattern)?;
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        Ok(())
    }

    /// The user enters a regex and we will derive a new column from that.
    /// If the user enter a capture group we will ask for a second input that shows how the capture group should be transformed.
    /// If the user doesn't enter a capture group we will just copy the regex match.
    pub fn regex_transform(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let pattern = inputs[0].to_owned();
        let regex = regex::Regex::new(&pattern)?;
        let contains_capture_pattern = regex.capture_names().len() > 1;
        let header = self.selected_column()?;

        log::error!("TODO implement capture groups again!!!");
        if let Some(transformation) = inputs.get(1) {
            if contains_capture_pattern && !transformation.is_empty() {
                // let transformation = if cfg!(debug_assertions) {
                //     TUI::get_user_input(r"${1}${2}")?
                // } else {
                // TUI::get_user_input(
                //     r"Enter transformation, e.g. '${first} ${second}' or '${1} ${2}' if un-named",
                // )?
                // };
                self.database.regex_capture_group_transform(
                    &self.view.table_name,
                    &pattern,
                    &header,
                    transformation,
                )?;
                return Ok(());
            }
        }
        self.database
            .regex_no_capture_group_transform(&self.view.table_name, &pattern, &header)?;
        Ok(())
    }

    pub fn copy(&mut self) -> AppResult<()> {
        self.database
            .copy(&self.view.table_name, &self.selected_column()?)?;
        self.ui.last_command = PreviousCommand::new(Command::Copy, None);
        Ok(())
    }

    pub(crate) fn edit_cell(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let header = self.selected_column()?;
        let id = self
            .view
            .rows
            .get(self.view.row)
            .and_then(|r| r.first())
            .map(|v| v.to_string())
            .ok_or_else(|| app_error_other!("No selected row"))?
            .parse::<i32>()
            .map_err(|e| app_error_other!(e.to_string()))?;
        let data = self.database.get_cell(&self.view.table_name, id, &header)?;
        let edit_result = inputs[0].to_owned();

        self.database
            .update_cell(&self.view.table_name, header.as_str(), id, &edit_result)?;
        Ok(())
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        let header = self.selected_column()?;
        if self.view.order_column.as_deref() == Some(&header) {
            self.view.ascending = !self.view.ascending;
        } else {
            self.view.order_column = Some(header);
            self.view.ascending = true;
        }
        self.view.row = 0;
        self.view.row_offset = 0;
        Ok(())
    }
    fn exact_search(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let pattern = inputs[0].to_owned();
        let header = self.selected_column()?;
        let current = self.view.row_offset + self.view.row as u32;
        match self.database.exact_search(
            &self.view.table_name,
            &self.ordering(),
            current,
            &header,
            &pattern,
        ) {
            Ok(position) => {
                let height = TUI::get_table_height()?.max(1);
                self.view.row = (position % height) as usize;
                self.view.row_offset = position - position % height;
                self.ui.last_command =
                    PreviousCommand::new(Command::ExactSearch, Some("Match found".to_string()));
            }
            Err(_) => {
                self.ui.last_command =
                    PreviousCommand::new(Command::ExactSearch, Some("No match found".to_string()));
            }
        }
        Ok(())
    }

    fn text_to_int(&mut self) -> AppResult<()> {
        self.ui.last_command = PreviousCommand::new(Command::TextToInt, None);
        self.database
            .text_to_int(&self.view.table_name, &self.selected_column()?)
    }

    fn int_to_text(&mut self) -> AppResult<()> {
        self.ui.last_command = PreviousCommand::new(Command::IntToText, None);
        self.database
            .int_to_text(&self.view.table_name, &self.selected_column()?)
    }

    fn delete_column(&mut self) -> Result<(), AppError> {
        let column = self.selected_column()?;
        let text = self
            .database
            .delete_column(&self.view.table_name, &column)?;
        if self.view.order_column.as_deref() == Some(&column) {
            self.view.order_column = None;
        }
        let count = self.database.count_headers(&self.view.table_name)? as usize;
        self.view.column = self.view.column.min(count.saturating_sub(1));
        self.ui.last_command = PreviousCommand::new(Command::DeleteColumn, Some(text));
        Ok(())
    }

    fn rename_column(&mut self, inputs: Vec<String>) -> Result<(), AppError> {
        let new_column = inputs[0].to_owned();
        let old_column = self.selected_column()?;
        self.database
            .rename_column(&self.view.table_name, &old_column, &new_column)?;
        if self.view.order_column.as_deref() == Some(&old_column) {
            self.view.order_column = Some(new_column);
        }
        self.ui.last_command = PreviousCommand::new(Command::RenameColumn, None);
        Ok(())
    }

    fn execute_queued_command(&mut self) -> AppResult<()> {
        if let Some(queued_command) = &self.ui.queued_command {
            // TODO: this is not good, but makes it a bit easier to read
            let inputs = queued_command.inputs.clone();
            let result: AppResult<()> = match queued_command.command {
                Command::RegexTransform => self.regex_transform(inputs),
                Command::Edit => self.edit_cell(inputs),
                // CREATE TABLE data2 AS SELECT firstname FROM data WHERE lastname = 'zenkert';
                Command::SqlQuery => self.sql_query(inputs),
                Command::RegexFilter => self.regex_filter(inputs),
                Command::RenameColumn => self.rename_column(inputs),
                Command::RenameTable => self.rename_table(inputs),
                Command::ExactSearch => self.exact_search(inputs),
                Command::Save => {
                    let filename = inputs[0].as_str();
                    let path = PathBuf::from(filename);
                    match path.extension().unwrap().to_str().unwrap() {
                        "csv" => savers::csv(&self.database, &self.view.table_name, &path),
                        "sqlite" | "db" | "sql" | "sqlite3" => {
                            savers::sqlite(&self.database, &path)
                        }
                        _ => Err(AppError::from("Unsupported file format")),
                    }
                }
                Command::MathOperation => {
                    self.database.math_operation(&self.view.table_name, inputs)
                }
                // _ => {
                //     log::error!("Command not implemented: {:?}", queued_command.command);
                //     Err(AppError::from("Command not implemented"))
                // }
                Command::Move(_)
                | Command::Quit
                | Command::Copy
                | Command::IllegalOperation
                | Command::None
                | Command::Sort
                | Command::NextTable
                | Command::PrevTable
                | Command::TextToInt
                | Command::IntToText
                | Command::DeleteTable
                | Command::ToggleMetadataTable
                | Command::DeleteColumn => {
                    log::error!(
                        "Non-queueable command executed as queued: {:?}",
                        queued_command.command
                    );
                    unreachable!("This command should not be queueable!!!");
                }
            };
        }
        Ok(())
    }

    fn rename_table(&mut self, inputs: Vec<String>) -> Result<(), AppError> {
        let new_table_name = inputs[0].to_owned();
        self.database
            .rename_table(&self.view.table_name, &new_table_name)?;
        self.view.table_name = new_table_name;
        self.ui.last_command = PreviousCommand::new(Command::RenameColumn, None);
        Ok(())
    }
    // Other methods from the Controller struct
}
