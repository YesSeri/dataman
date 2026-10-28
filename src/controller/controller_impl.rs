use crate::app_error_other;
use crate::controller::command::{Command, PreviousCommand};
use crate::controller::input::{self, InputMode};
use crate::error::{AppError, AppResult};
use crate::io::savers;
use crate::model::database::{Database, UndoRecord, INTERNAL_PREFIX};
use crate::model::datarow::{DataItem, DataTable};
use crate::navigation::Direction;
use crate::tui::TUI;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::{collections::VecDeque, path::PathBuf};

use super::command::QueuedCommand;
use super::input::StateMachine;

const POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(3000);
const HISTORY_LIMIT: usize = 100;

#[derive(Debug)]
pub struct Controller {
    pub(crate) database: Database,
    pub(crate) ui: UiState,
    pub(crate) view: ViewState,
    history: History,
}

#[derive(Debug, Clone)]
struct ViewSnapshot {
    table_name: String,
    column: usize,
    row: usize,
    row_offset: u32,
    order_column: Option<String>,
    ascending: bool,
    table_before_metadata: Option<String>,
}

#[derive(Debug)]
struct HistoryEntry {
    record: UndoRecord,
    before: ViewSnapshot,
    after: ViewSnapshot,
}

#[derive(Debug, Default)]
struct History {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
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
            history: History::default(),
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

    fn view_snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            table_name: self.view.table_name.clone(),
            column: self.view.column,
            row: self.view.row,
            row_offset: self.view.row_offset,
            order_column: self.view.order_column.clone(),
            ascending: self.view.ascending,
            table_before_metadata: self.view.table_before_metadata.clone(),
        }
    }

    fn restore_view(&mut self, snapshot: ViewSnapshot) {
        self.view.table_name = snapshot.table_name;
        self.view.column = snapshot.column;
        self.view.row = snapshot.row;
        self.view.row_offset = snapshot.row_offset;
        self.view.order_column = snapshot.order_column;
        self.view.ascending = snapshot.ascending;
        self.view.table_before_metadata = snapshot.table_before_metadata;
    }

    fn record_history(&mut self, record: UndoRecord, before: ViewSnapshot) {
        for entry in self.history.redo.drain(..) {
            if let Err(err) = self.database.discard_hidden_record(&entry.record) {
                log::warn!("Could not discard redo history artifact: {err}");
            }
        }
        let after = self.view_snapshot();
        self.history.undo.push_back(HistoryEntry {
            record,
            before,
            after,
        });
        while self.history.undo.len() > HISTORY_LIMIT {
            if let Some(entry) = self.history.undo.pop_front() {
                if let Err(err) = self.database.discard_hidden_record(&entry.record) {
                    log::warn!("Could not discard expired history artifact: {err}");
                }
            }
        }
    }

    fn clear_history(&mut self) {
        for entry in self
            .history
            .undo
            .drain(..)
            .chain(self.history.redo.drain(..))
        {
            if let Err(err) = self.database.discard_hidden_record(&entry.record) {
                log::warn!("Could not discard history artifact: {err}");
            }
        }
    }

    fn undo(&mut self) -> AppResult<()> {
        let Some(mut entry) = self.history.undo.pop_back() else {
            self.ui.last_command =
                PreviousCommand::new(Command::Undo, Some("Nothing to undo".into()));
            return Ok(());
        };
        if let Err(err) = self.database.toggle_history_record(&mut entry.record) {
            self.history.undo.push_back(entry);
            return Err(err);
        }
        self.restore_view(entry.before.clone());
        self.history.redo.push_back(entry);
        self.ui.last_command =
            PreviousCommand::new(Command::Undo, Some("Undid last command".into()));
        Ok(())
    }

    fn redo(&mut self) -> AppResult<()> {
        let Some(mut entry) = self.history.redo.pop_back() else {
            self.ui.last_command =
                PreviousCommand::new(Command::Redo, Some("Nothing to redo".into()));
            return Ok(());
        };
        if let Err(err) = self.database.toggle_history_record(&mut entry.record) {
            self.history.redo.push_back(entry);
            return Err(err);
        }
        self.restore_view(entry.after.clone());
        self.history.undo.push_back(entry);
        self.ui.last_command = PreviousCommand::new(Command::Redo, Some("Redid command".into()));
        Ok(())
    }

    fn dispatch_history_command(&mut self, command: Command) -> AppResult<()> {
        match command {
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            _ => unreachable!("only undo/redo commands are dispatched here"),
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
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        if table == "table_of_tables" {
            return Err(app_error_other!("The metadata table cannot be deleted"));
        }
        let hidden_name = self.database.delete_table(&table)?;
        self.change_table(false)?;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: table,
                hidden_name,
                is_visible: false,
            },
            before,
        );
        Ok(())
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
                        Command::Undo | Command::Redo => {
                            self.dispatch_history_command(command.clone())
                        } // Command::Join(_) => todo!(),
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
                        if self.ui.last_command.command != Command::IllegalOperation
                            && queued_command.command == Command::SqlQuery
                        {
                            self.ui.last_command = PreviousCommand::new(
                                Command::SqlQuery,
                                Some(
                                    "SQL executed; undo history was cleared (SQL is not undoable)"
                                        .into(),
                                ),
                            );
                        } else if self.ui.last_command.command != Command::IllegalOperation {
                            let message = self.ui.last_command.message.clone().or_else(|| {
                                Some("Completed".to_owned())
                            });
                            self.ui.last_command = PreviousCommand::new(queued_command.command, message);
                        }
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
                        self.clear_history();
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
        let before = self.view_snapshot();
        let pattern = inputs[0].to_owned();
        let header = self.selected_column()?;
        let filtered_table =
            self.database
                .regex_filter(&self.view.table_name, &header, &pattern)?;
        let hidden_name = self.database.internal_table_name()?;
        self.view.table_name = filtered_table.clone();
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: filtered_table,
                hidden_name,
                is_visible: true,
            },
            before,
        );
        Ok(())
    }

    /// The user enters a regex and we will derive a new column from that.
    /// If the user enter a capture group we will ask for a second input that shows how the capture group should be transformed.
    /// If the user doesn't enter a capture group we will just copy the regex match.
    pub fn regex_transform(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
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
                self.record_added_column(&table, &old_headers, before)?;
                return Ok(());
            }
        }
        self.database
            .regex_no_capture_group_transform(&self.view.table_name, &pattern, &header)?;
        self.record_added_column(&table, &old_headers, before)?;
        Ok(())
    }

    fn record_added_column(
        &mut self,
        table: &str,
        old_headers: &[String],
        before: ViewSnapshot,
    ) -> AppResult<()> {
        let new_column = self
            .database
            .get_headers(table)?
            .into_iter()
            .find(|header| !old_headers.contains(header))
            .ok_or_else(|| app_error_other!("Operation did not create a column"))?;
        let hidden_name = self.database.internal_column_name(table)?;
        self.record_history(
            UndoRecord::Visibility {
                table: Some(table.to_owned()),
                visible_name: new_column,
                hidden_name,
                is_visible: true,
            },
            before,
        );
        Ok(())
    }

    pub fn copy(&mut self) -> AppResult<()> {
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        self.database
            .copy(&self.view.table_name, &self.selected_column()?)?;
        self.record_added_column(&table, &old_headers, before)?;
        self.ui.last_command = PreviousCommand::new(Command::Copy, None);
        Ok(())
    }

    pub(crate) fn edit_cell(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let before_view = self.view_snapshot();
        let header = self.selected_column()?;
        if header == "id" {
            return Err(app_error_other!("The id column is read-only"));
        }
        let table = self.view.table_name.clone();
        let id = self
            .view
            .rows
            .get(self.view.row)
            .and_then(|r| r.first())
            .map(|v| v.to_string())
            .ok_or_else(|| app_error_other!("No selected row"))?
            .parse::<i32>()
            .map_err(|e| app_error_other!(e.to_string()))?;
        let data = self.database.get_cell_item(&table, id, &header)?;
        let edit_result = inputs[0].to_owned();

        self.database
            .update_cell(&self.view.table_name, header.as_str(), id, &edit_result)?;
        self.record_history(
            UndoRecord::CellEdit {
                table,
                column: header,
                id,
                before: data,
                after: DataItem::Text(edit_result),
                is_after: true,
            },
            before_view,
        );
        Ok(())
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        let before = self.view_snapshot();
        let header = self.selected_column()?;
        if self.view.order_column.as_deref() == Some(&header) {
            self.view.ascending = !self.view.ascending;
        } else {
            self.view.order_column = Some(header);
            self.view.ascending = true;
        }
        self.view.row = 0;
        self.view.row_offset = 0;
        self.record_history(UndoRecord::ViewChange, before);
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
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        self.ui.last_command = PreviousCommand::new(Command::TextToInt, None);
        self.database
            .text_to_int(&table, &self.selected_column()?)?;
        self.record_added_column(&table, &old_headers, before)
    }

    fn int_to_text(&mut self) -> AppResult<()> {
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        self.ui.last_command = PreviousCommand::new(Command::IntToText, None);
        self.database
            .int_to_text(&table, &self.selected_column()?)?;
        self.record_added_column(&table, &old_headers, before)
    }

    fn delete_column(&mut self) -> Result<(), AppError> {
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let column = self.selected_column()?;
        let hidden_name = self
            .database
            .delete_column(&self.view.table_name, &column)?;
        if self.view.order_column.as_deref() == Some(&column) {
            self.view.order_column = None;
        }
        let count = self.database.count_headers(&self.view.table_name)? as usize;
        self.view.column = self.view.column.min(count.saturating_sub(1));
        self.ui.last_command = PreviousCommand::new(
            Command::DeleteColumn,
            Some(format!("Deleted column {column}")),
        );
        self.record_history(
            UndoRecord::Visibility {
                table: Some(table),
                visible_name: column,
                hidden_name,
                is_visible: false,
            },
            before,
        );
        Ok(())
    }

    fn rename_column(&mut self, inputs: Vec<String>) -> Result<(), AppError> {
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let new_column = inputs[0].to_owned();
        if new_column.starts_with(INTERNAL_PREFIX) {
            return Err(app_error_other!(
                "Column names using Dataman's reserved undo prefix are not supported"
            ));
        }
        let old_column = self.selected_column()?;
        self.database
            .rename_column(&self.view.table_name, &old_column, &new_column)?;
        if self.view.order_column.as_deref() == Some(&old_column) {
            self.view.order_column = Some(new_column.clone());
        }
        self.ui.last_command = PreviousCommand::new(Command::RenameColumn, None);
        self.record_history(
            UndoRecord::RenameColumn {
                table,
                before: old_column,
                after: new_column,
                is_after: true,
            },
            before,
        );
        Ok(())
    }

    fn math_operation(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let before = self.view_snapshot();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        self.database.math_operation(&table, inputs)?;
        self.record_added_column(&table, &old_headers, before)
    }

    fn execute_queued_command(&mut self) -> AppResult<()> {
        if let Some(queued_command) = self.ui.queued_command.clone() {
            // TODO: this is not good, but makes it a bit easier to read
            let inputs = queued_command.inputs;
            let command = queued_command.command;
            let result: AppResult<()> = match command {
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
                Command::MathOperation => self.math_operation(inputs),
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
                | Command::Undo
                | Command::Redo
                | Command::ToggleMetadataTable
                | Command::DeleteColumn => {
                    log::error!("Non-queueable command executed as queued: {:?}", command);
                    unreachable!("This command should not be queueable!!!");
                }
            };
            if let Err(err) = result {
                self.ui.last_command =
                    PreviousCommand::new(Command::IllegalOperation, Some(err.to_string()));
                return Ok(());
            }
            if command == Command::SqlQuery {
                self.clear_history();
            }
        }
        Ok(())
    }

    fn rename_table(&mut self, inputs: Vec<String>) -> Result<(), AppError> {
        let before = self.view_snapshot();
        let new_table_name = inputs[0].to_owned();
        if self.view.table_name == "table_of_tables" {
            return Err(app_error_other!("The metadata table cannot be renamed"));
        }
        if new_table_name.starts_with(INTERNAL_PREFIX) {
            return Err(app_error_other!(
                "Table names using Dataman's reserved undo prefix are not supported"
            ));
        }
        let old_table_name = self.view.table_name.clone();
        self.database
            .rename_table(&old_table_name, &new_table_name)?;
        self.view.table_name = new_table_name;
        self.ui.last_command = PreviousCommand::new(Command::RenameTable, None);
        self.record_history(
            UndoRecord::RenameTable {
                before: old_table_name,
                after: self.view.table_name.clone(),
                is_after: true,
            },
            before,
        );
        Ok(())
    }
    // Other methods from the Controller struct
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use rusqlite::Connection;

    fn controller() -> Controller {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO t (id, name) VALUES (1, 'abc'), (2, 'def');",
                [],
            )
            .unwrap();
        let database = Database::new(connection).unwrap();
        let mut controller = Controller::new(database);
        controller.view.table_name = "t".into();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        controller
    }

    fn raw_headers(controller: &Controller, table: &str) -> Vec<String> {
        let mut stmt = controller
            .database
            .connection
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap();
        stmt.query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    }

    #[test]
    fn cell_edit_undoes_and_redoes_previous_typed_value() {
        let mut controller = controller();
        controller.edit_cell(vec!["changed".into()]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "changed"
        );

        controller.undo().unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        controller.redo().unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "changed"
        );
    }

    #[test]
    fn editing_the_row_id_is_rejected_without_history() {
        let mut controller = controller();
        controller.view.column = 0;
        assert!(controller.edit_cell(vec!["10".into()]).is_err());
        assert_eq!(controller.database.count_rows("t"), Some(2));
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        assert!(controller.history.undo.is_empty());
    }

    #[test]
    fn rename_column_undoes_and_redoes() {
        let mut controller = controller();
        controller.rename_column(vec!["label".into()]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"label".into()));
        controller.undo().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name".into()));
        controller.redo().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"label".into()));
    }

    #[test]
    fn generated_column_is_hidden_and_restored_by_history() {
        let mut controller = controller();
        controller.copy().unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name_copy").unwrap(),
            "abc"
        );
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
        assert!(!controller
            .database
            .get(10, 0, "t", "")
            .unwrap()
            .0
            .contains(&"name_copy".into()));
        controller.redo().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
    }

    #[test]
    fn regex_transform_is_undoable_and_redoable() {
        let mut controller = controller();
        controller.regex_transform(vec!["a".into()]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_tform".into()));
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_tform".into()));
        controller.redo().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_tform".into()));
    }

    #[test]
    fn type_conversion_columns_are_undoable_and_redoable() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("ALTER TABLE t ADD COLUMN count INTEGER; UPDATE t SET count = 42;")
            .unwrap();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 2;

        controller.int_to_text().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"TEXT_count".into()));
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"TEXT_count".into()));
        controller.redo().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"TEXT_count".into()));

        controller.view.column = 2;
        controller.text_to_int().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
        controller.redo().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
    }

    #[test]
    fn math_generated_column_is_undoable_and_redoable() {
        let mut controller = controller();
        controller.math_operation(vec!["id + 10".into()]).unwrap();
        assert_eq!(
            controller
                .database
                .get_cell_item("t", 1, "math_expr")
                .unwrap(),
            DataItem::Integer(11)
        );
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"math_expr".into()));
        controller.redo().unwrap();
        assert_eq!(
            controller
                .database
                .get_cell_item("t", 1, "math_expr")
                .unwrap(),
            DataItem::Integer(11)
        );
    }

    #[test]
    fn undo_sort_before_undoing_derived_column() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("ALTER TABLE t ADD COLUMN count TEXT; UPDATE t SET count = '5';")
            .unwrap();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 2;

        controller.text_to_int().unwrap();
        let derived_column = controller.view.headers.len();
        controller.view.column = derived_column;
        controller.sort().unwrap();

        assert_eq!(
            controller.view.order_column.as_deref(),
            Some("INTEGER_count")
        );
        controller.undo().unwrap();
        assert_eq!(controller.view.order_column.as_deref(), Some("id"));
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));

        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
    }

    #[test]
    fn deleted_column_is_retained_and_restored() {
        let mut controller = controller();
        controller.delete_column().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name".into()));
        controller.undo().unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        controller.redo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name".into()));
    }

    #[test]
    fn filtered_table_undoes_and_redoes_view_transition() {
        let mut controller = controller();
        controller.regex_filter(vec!["a".into()]).unwrap();
        let filtered = controller.view.table_name.clone();
        assert_eq!(controller.database.count_rows(&filtered), Some(1));
        controller.undo().unwrap();
        assert_eq!(controller.view.table_name, "t");
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&filtered));
        controller.redo().unwrap();
        assert_eq!(controller.view.table_name, filtered);
        assert_eq!(controller.database.count_rows(&filtered), Some(1));
    }

    #[test]
    fn deleted_table_is_retained_and_restored() {
        let mut controller = controller();
        controller.delete_table().unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        controller.undo().unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        assert_eq!(controller.view.table_name, "t");
        controller.redo().unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
    }

    #[test]
    fn renamed_table_undoes_and_redoes() {
        let mut controller = controller();
        controller.rename_table(vec!["renamed".into()]).unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"renamed".into()));
        controller.undo().unwrap();
        assert_eq!(controller.view.table_name, "t");
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        controller.redo().unwrap();
        assert_eq!(controller.view.table_name, "renamed");
    }

    #[test]
    fn new_edit_after_undo_discards_redo() {
        let mut controller = controller();
        controller.edit_cell(vec!["first".into()]).unwrap();
        controller.undo().unwrap();
        controller.edit_cell(vec!["second".into()]).unwrap();
        controller.redo().unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "second"
        );
    }

    #[test]
    fn configured_undo_redo_keys_reach_history_dispatch() {
        let mut controller = controller();
        controller.edit_cell(vec!["changed".into()]).unwrap();
        let undo = Command::from(crossterm::event::KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
        ));
        controller.dispatch_history_command(undo).unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");

        let redo = Command::from(crossterm::event::KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        ));
        controller.dispatch_history_command(redo).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "changed"
        );
    }

    #[test]
    fn new_command_removes_hidden_redo_artifacts() {
        let mut controller = controller();
        controller.copy().unwrap();
        controller.undo().unwrap();
        assert!(raw_headers(&controller, "t")
            .iter()
            .any(|header| header.starts_with(INTERNAL_PREFIX)));
        controller.edit_cell(vec!["new value".into()]).unwrap();
        assert!(!raw_headers(&controller, "t")
            .iter()
            .any(|header| header.starts_with(INTERNAL_PREFIX)));
        assert!(controller.history.redo.is_empty());
    }

    #[test]
    fn successful_sql_clears_undo_history() {
        let mut controller = controller();
        controller.edit_cell(vec!["before sql".into()]).unwrap();
        let mut queued = QueuedCommand::new(Command::SqlQuery);
        queued
            .inputs
            .push("UPDATE t SET name = 'from sql' WHERE id = 1;".into());
        controller.ui.queued_command = Some(queued);
        controller.execute_queued_command().unwrap();
        controller.ui.queued_command = None;
        assert!(controller.history.undo.is_empty());
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "from sql"
        );
        controller.undo().unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "from sql"
        );
    }

    #[test]
    fn failed_rename_does_not_enter_history() {
        let mut controller = controller();
        assert!(controller.rename_column(vec!["id".into()]).is_err());
        assert!(controller.history.undo.is_empty());
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name".into()));
    }

    #[test]
    fn history_is_bounded_to_one_hundred_entries() {
        let mut controller = controller();
        for index in 0..101 {
            controller
                .edit_cell(vec![format!("value-{index}")])
                .unwrap();
        }
        assert_eq!(controller.history.undo.len(), HISTORY_LIMIT);
        for _ in 0..HISTORY_LIMIT {
            controller.undo().unwrap();
        }
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "value-0"
        );
    }

    #[test]
    fn undo_preserves_original_sqlite_value_type() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, count INTEGER);",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO t (id, count) VALUES (1, 42);", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        let mut controller = Controller::new(database);
        controller.view.table_name = "t".into();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        controller.edit_cell(vec!["not-a-number".into()]).unwrap();
        controller.undo().unwrap();
        assert_eq!(
            controller.database.get_cell_item("t", 1, "count").unwrap(),
            DataItem::Integer(42)
        );
    }

    #[test]
    fn sqlite_export_omits_hidden_history_artifacts() {
        let mut controller = controller();
        controller.delete_table().unwrap();
        let path =
            std::env::temp_dir().join(format!("dataman-undo-export-{}.sqlite", std::process::id()));
        controller.database.backup_db(&path).unwrap();
        let exported = Connection::open(&path).unwrap();
        let internal_count: i64 = exported
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '__dataman_undo_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(internal_count, 0);
        drop(exported);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn csv_export_omits_hidden_history_columns() {
        let mut controller = controller();
        controller.copy().unwrap();
        controller.undo().unwrap();
        let path = std::env::temp_dir().join(format!(
            "dataman-undo-csv-export-{}.csv",
            std::process::id()
        ));
        savers::csv(&controller.database, "t", &path).unwrap();
        let mut reader = csv::Reader::from_path(&path).unwrap();
        let headers = reader.headers().unwrap().clone();
        assert!(!headers
            .iter()
            .any(|header| header.starts_with(INTERNAL_PREFIX)));
        assert!(headers.iter().any(|header| header == "name"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn startup_removes_stale_session_artifacts() {
        let path =
            std::env::temp_dir().join(format!("dataman-undo-stale-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, __dataman_undo_column_0 TEXT);",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "CREATE TABLE __dataman_undo_table_0 (id INTEGER PRIMARY KEY);",
                    [],
                )
                .unwrap();
        }
        let database = Database::new(Connection::open(&path).unwrap()).unwrap();
        assert_eq!(database.get_headers("t").unwrap(), vec!["id", "name"]);
        assert!(!database
            .get_table_names()
            .unwrap()
            .iter()
            .any(|name| name.starts_with(INTERNAL_PREFIX)));
        drop(database);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn pruned_deleted_table_artifact_is_physically_removed() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);\
                 INSERT INTO t (id, name) VALUES (1, 'a');\
                 CREATE TABLE u (id INTEGER PRIMARY KEY, name TEXT);\
                 INSERT INTO u (id, name) VALUES (1, 'b');",
            )
            .unwrap();
        let database = Database::new(connection).unwrap();
        let mut controller = Controller::new(database);
        controller.view.table_name = "t".into();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        controller.delete_table().unwrap();
        assert_eq!(controller.view.table_name, "u");
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        for index in 0..HISTORY_LIMIT {
            controller
                .edit_cell(vec![format!("value-{index}")])
                .unwrap();
        }
        let internal_tables: i64 = controller
            .database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name GLOB '__dataman_undo_*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(internal_tables, 0);
        assert_eq!(controller.history.undo.len(), HISTORY_LIMIT);
    }

    #[test]
    fn undo_and_redo_failures_leave_their_history_entries_available() {
        {
            let mut controller = controller();
            controller.edit_cell(vec!["changed".into()]).unwrap();
            controller
                .database
                .execute_batch("ALTER TABLE t DROP COLUMN name;")
                .unwrap();
            assert!(controller.undo().is_err());
            assert_eq!(controller.history.undo.len(), 1);
        }

        let mut redo_controller = controller();
        redo_controller.edit_cell(vec!["changed".into()]).unwrap();
        redo_controller.undo().unwrap();
        redo_controller
            .database
            .execute_batch("ALTER TABLE t DROP COLUMN name;")
            .unwrap();
        assert!(redo_controller.redo().is_err());
        assert_eq!(redo_controller.history.redo.len(), 1);
    }
}
