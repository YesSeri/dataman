use super::selection::Selection;
use crate::app_error_other;
use crate::controller::command::{Command, PreviousCommand};
use crate::controller::input::{self, InputMode};
use crate::error::{AppError, AppResult};
use crate::io::savers;
use crate::model::database::{Aggregator, Database, JoinKind, UndoRecord, INTERNAL_PREFIX};
use crate::model::datarow::{DataItem, DataTable};
use crate::navigation::Direction;
use crate::tui::TUI;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
};

use super::command::QueuedCommand;
use super::input::StateMachine;

const HISTORY_LIMIT: usize = 100;

#[derive(Debug)]
pub struct Controller {
    pub(crate) database: Database,
    pub(crate) ui: UiState,
    pub(crate) view: ViewState,
    history: History,
    pub(crate) selected_rows: Selection,
    pub(crate) selected_sheets: HashSet<String>,
    column_layouts: HashMap<String, ColumnLayout>,
    sheet_order: Vec<String>,
    pub(crate) join_keys: HashMap<String, Vec<String>>,
    table_sources: HashMap<String, Vec<String>>,
    aggregators: HashMap<String, Vec<(String, Aggregator)>>,
    frequency_sources: HashMap<String, (String, String)>,
    frequency_selection_baselines: HashMap<String, Selection>,
    drilldown_return_sheet: Option<String>,
    sheet_views: HashMap<String, SheetView>,
    previous_sheet: Option<String>,
    page_key: Option<(String, String, u32, u32, u64, ColumnLayout)>,
    source_page: Option<SourcePage>,
    pub(super) table_height: u32,
    pub(super) defer_refresh: bool,
    pub(crate) command_history: VecDeque<LoggedCommand>,
}

#[derive(Debug, Clone)]
struct SheetView {
    column: usize,
    row: usize,
    row_offset: u32,
    order_column: Option<String>,
    ascending: bool,
    selection: Selection,
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
    layout: ColumnLayout,
    selected_rows: Selection,
    selected_sheets: HashSet<String>,
    sheet_order: Vec<String>,
    join_keys: HashMap<String, Vec<String>>,
    table_sources: HashMap<String, Vec<String>>,
    aggregators: HashMap<String, Vec<(String, Aggregator)>>,
}

#[derive(Debug)]
struct HistoryEntry {
    record: UndoRecord,
    before: ViewSnapshot,
    after: ViewSnapshot,
}

#[derive(Debug, Clone)]
pub(crate) struct LoggedCommand {
    pub(crate) command: Command,
    pub(crate) inputs: Vec<String>,
    pub(crate) table: String,
    snapshot: ViewSnapshot,
    selected_rows: Selection,
}

#[derive(Debug, Default)]
struct History {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ColumnLayout {
    order: Vec<String>,
    hidden: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ViewState {
    pub(crate) table_name: String,
    pub(crate) column: usize,
    pub(crate) row: usize,
    pub(crate) row_offset: u32,
    pub(crate) order_column: Option<String>,
    pub(crate) ascending: bool,
    pub(crate) headers: Vec<String>,
    pub(crate) rows: Arc<Vec<Vec<DataItem>>>,
    pub(crate) column_indexes: Vec<usize>,
    pub(crate) table_before_metadata: Option<String>,
}

#[derive(Debug)]
struct SourcePage {
    key: (String, String, u32, u32, u64),
    headers: Vec<String>,
    rows: Arc<Vec<Vec<DataItem>>>,
}

/// Terminal interaction state kept out of the SQLite-backed data model.
#[derive(Debug, Clone)]
pub(crate) struct UiState {
    pub(crate) input: String,
    pub(crate) character_index: usize,
    pub(crate) last_command: PreviousCommand,
    pub(crate) queued_command: Option<QueuedCommand>,
    pub(crate) input_mode_state_machine: StateMachine,
    pub(crate) history_view: bool,
    pub(crate) history_cursor: usize,
}

impl Controller {
    pub fn new(database: Database) -> Self {
        let mut sheet_order = database.get_table_names().unwrap_or_default();
        sheet_order.retain(|name| name != "table_of_tables" && !name.starts_with(INTERNAL_PREFIX));
        let table_name = sheet_order.first().cloned().unwrap_or_default();
        Self {
            database,
            history: History::default(),
            selected_rows: Selection::default(),
            selected_sheets: HashSet::new(),
            column_layouts: HashMap::new(),
            sheet_order,
            frequency_sources: HashMap::new(),
            frequency_selection_baselines: HashMap::new(),
            drilldown_return_sheet: None,
            join_keys: HashMap::new(),
            table_sources: HashMap::new(),
            aggregators: HashMap::new(),
            sheet_views: HashMap::new(),
            previous_sheet: None,
            page_key: None,
            source_page: None,
            table_height: 20,
            defer_refresh: false,
            command_history: VecDeque::new(),
            view: ViewState {
                table_name,
                column: 0,
                row: 0,
                row_offset: 0,
                order_column: Some("id".into()),
                ascending: true,
                headers: vec![],
                rows: Arc::default(),
                column_indexes: vec![],
                table_before_metadata: None,
            },
            ui: UiState {
                input: String::new(),
                character_index: 0,
                last_command: PreviousCommand::new(Command::None, None),
                queued_command: None,
                input_mode_state_machine: StateMachine::new(),
                history_view: false,
                history_cursor: 0,
            },
        }
    }

    fn view_snapshot(&self) -> ViewSnapshot {
        let layout = self
            .column_layouts
            .get(&self.view.table_name)
            .cloned()
            .unwrap_or_default();
        ViewSnapshot {
            table_name: self.view.table_name.clone(),
            column: self.view.column,
            row: self.view.row,
            row_offset: self.view.row_offset,
            order_column: self.view.order_column.clone(),
            ascending: self.view.ascending,
            table_before_metadata: self.view.table_before_metadata.clone(),
            layout,
            selected_rows: self.selected_rows.clone(),
            selected_sheets: self.selected_sheets.clone(),
            sheet_order: self.sheet_order.clone(),
            join_keys: self.join_keys.clone(),
            table_sources: self.table_sources.clone(),
            aggregators: self.aggregators.clone(),
        }
    }

    fn restore_view(&mut self, snapshot: ViewSnapshot) {
        let layout = snapshot.layout.clone();
        self.selected_rows = snapshot.selected_rows.clone();
        self.selected_sheets = snapshot.selected_sheets.clone();
        self.sheet_order = snapshot.sheet_order.clone();
        self.join_keys = snapshot.join_keys.clone();
        self.table_sources = snapshot.table_sources.clone();
        self.aggregators = snapshot.aggregators.clone();
        self.view.table_name = snapshot.table_name;
        self.view.column = snapshot.column;
        self.view.row = snapshot.row;
        self.view.row_offset = snapshot.row_offset;
        self.view.order_column = snapshot.order_column;
        self.view.ascending = snapshot.ascending;
        self.view.table_before_metadata = snapshot.table_before_metadata;
        self.column_layouts
            .insert(self.view.table_name.clone(), layout);
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

    fn log_command(&mut self, command: Command, inputs: Vec<String>, snapshot: ViewSnapshot) {
        if matches!(
            command,
            Command::None
                | Command::IllegalOperation
                | Command::Quit
                | Command::Move(_)
                | Command::NextTable
                | Command::PrevTable
                | Command::PreviousSheet
                | Command::Save
                | Command::SqlQuery
                | Command::Undo
                | Command::Redo
                | Command::History
                | Command::HistoryReplay
                | Command::HistoryClose
                | Command::ToggleSelection
                | Command::ToggleJoinKey
                | Command::DeleteSelected
                | Command::CopySelected
                | Command::ClearSelection
        ) {
            return;
        }
        self.command_history.push_back(LoggedCommand {
            command,
            inputs,
            table: snapshot.table_name.clone(),
            snapshot,
            selected_rows: self.selected_rows.clone(),
        });
        while self.command_history.len() > HISTORY_LIMIT {
            self.command_history.pop_front();
        }
        self.ui.history_cursor = self.command_history.len().saturating_sub(1);
    }

    fn replay_history(&mut self) -> AppResult<()> {
        let entry = self
            .command_history
            .get(self.ui.history_cursor)
            .cloned()
            .ok_or_else(|| app_error_other!("No command selected"))?;
        self.restore_view(entry.snapshot.clone());
        self.selected_rows = entry.selected_rows.clone();
        self.refresh_page(100)?;
        let result = match entry.command {
            Command::Copy => self.copy(),
            Command::Sort => self.sort(),
            Command::TextToInt => self.text_to_int(),
            Command::IntToText => self.int_to_text(),
            Command::DeleteColumn => self.delete_column(),
            Command::DeleteTable => self.delete_table(),
            Command::RenameColumn => self.rename_column(entry.inputs.clone()),
            Command::RenameTable => self.rename_table(entry.inputs.clone()),
            Command::Edit => self.edit_cell(entry.inputs.clone()),
            Command::RegexTransform => self.regex_transform(entry.inputs.clone()),
            Command::RegexFilter => self.regex_filter(entry.inputs.clone()),
            Command::MathOperation => self.math_operation(entry.inputs.clone()),
            Command::ExactSearch => self.exact_search(entry.inputs.clone()),
            Command::Join => self.join_table(entry.inputs.clone()),
            Command::CombineTables => self.combine_selected_tables(entry.inputs.clone()),
            Command::Group => self.create_grouped_sheet(),
            Command::Pivot => self.create_pivot_sheet(),
            Command::ConfigureAggregator => self.configure_aggregator(entry.inputs.clone()),
            Command::Summary => self.show_analysis(false),
            Command::Frequency => self.show_analysis(true),
            Command::BulkEdit => self.bulk_edit(entry.inputs.clone()),
            Command::HideColumn => self.hide_selected_column(),
            Command::ShowColumns => {
                self.show_all_columns();
                Ok(())
            }
            Command::ColumnLeft => self.move_column(true),
            Command::ColumnRight => self.move_column(false),
            _ => Err(app_error_other!("This command cannot be replayed")),
        };
        match result {
            Ok(()) => {
                self.ui.history_view = false;
                self.ui.last_command =
                    PreviousCommand::new(entry.command, Some("Replayed command".into()));
                Ok(())
            }
            Err(err) => {
                self.ui.last_command =
                    PreviousCommand::new(Command::IllegalOperation, Some(err.to_string()));
                Err(err)
            }
        }
    }

    pub(super) fn clear_history(&mut self) {
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
        if self.view.table_name == "table_of_tables" {
            self.refresh_sheet_list()?;
        }
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
        if self.view.table_name == "table_of_tables" {
            self.refresh_sheet_list()?;
        }
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
        if self.view.table_name == "table_of_tables" {
            let clauses = self
                .sheet_order
                .iter()
                .enumerate()
                .map(|(position, name)| {
                    format!("WHEN '{}' THEN {position}", name.replace('\'', "''"))
                })
                .collect::<Vec<_>>()
                .join(" ");
            return format!("ORDER BY CASE \"table_name\" {clauses} ELSE 2147483647 END ASC");
        }
        match &self.view.order_column {
            Some(column) => format!(
                r#"ORDER BY "{}" {}"#,
                column.replace('"', "\"\""),
                if self.view.ascending { "ASC" } else { "DESC" }
            ),
            None => String::new(),
        }
    }

    fn selected_column(&self) -> AppResult<String> {
        self.view
            .headers
            .get(self.view.column)
            .cloned()
            .ok_or_else(|| app_error_other!("Could not get header"))
    }

    fn move_cursor(&mut self, direction: Direction, height: u32) -> AppResult<()> {
        match direction {
            Direction::Left => {
                let count = self.view.headers.len();
                if count > 0 {
                    self.view.column = (self.view.column + count - 1) % count;
                }
            }
            Direction::Right => {
                let count = self.view.headers.len();
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

    fn hide_selected_column(&mut self) -> AppResult<()> {
        let name = self.selected_column()?;
        if name == "id" {
            return Err(app_error_other!(
                "The id column is pinned and cannot be hidden"
            ));
        }
        self.column_layouts
            .entry(self.view.table_name.clone())
            .or_default()
            .hidden
            .insert(name);
        self.view.column = self.view.column.saturating_sub(1);
        Ok(())
    }

    fn show_all_columns(&mut self) {
        self.column_layouts
            .entry(self.view.table_name.clone())
            .or_default()
            .hidden
            .clear();
    }

    fn move_column(&mut self, left: bool) -> AppResult<()> {
        let name = self.selected_column()?;
        if name == "id" {
            return Err(app_error_other!("The id column stays first"));
        }
        let visible = self.view.headers.clone();
        let position = self.view.column;
        let target = if left {
            position.saturating_sub(1)
        } else {
            (position + 1).min(visible.len().saturating_sub(1))
        };
        if target == 0 || target == position {
            return Ok(());
        }
        let layout = self
            .column_layouts
            .entry(self.view.table_name.clone())
            .or_default();
        let current_index = layout
            .order
            .iter()
            .position(|header| header == &name)
            .ok_or_else(|| app_error_other!("Column layout is out of date"))?;
        let target_index = layout
            .order
            .iter()
            .position(|header| header == &visible[target])
            .ok_or_else(|| app_error_other!("Column layout is out of date"))?;
        layout.order.swap(current_index, target_index);
        self.view.column = target;
        Ok(())
    }

    fn change_table(&mut self, next: bool) -> AppResult<()> {
        let mut names = self.database.get_table_names()?;
        names.retain(|name| name != "table_of_tables");
        self.sheet_order.retain(|name| names.contains(name));
        for name in &names {
            if !self.sheet_order.contains(name) {
                self.sheet_order.push(name.clone());
            }
        }
        names = self.sheet_order.clone();
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
            self.switch_sheet(names[target].clone())?;
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

    fn switch_sheet(&mut self, target: String) -> AppResult<()> {
        if target == self.view.table_name {
            return Ok(());
        }
        if self
            .drilldown_return_sheet
            .as_ref()
            .is_some_and(|return_to| return_to != &target)
        {
            self.drilldown_return_sheet = None;
        }
        let current = self.view.table_name.clone();
        self.sheet_views.insert(
            current.clone(),
            SheetView {
                column: self.view.column,
                row: self.view.row,
                row_offset: self.view.row_offset,
                order_column: self.view.order_column.clone(),
                ascending: self.view.ascending,
                selection: self.selected_rows.clone(),
            },
        );
        self.previous_sheet = Some(current);
        self.view.table_name = target.clone();
        if let Some(saved) = self.sheet_views.get(&target).cloned() {
            self.view.column = saved.column;
            self.view.row = saved.row;
            self.view.row_offset = saved.row_offset;
            self.view.order_column = saved.order_column;
            self.view.ascending = saved.ascending;
            self.selected_rows = saved.selection;
        } else {
            self.view.column = 0;
            self.view.row = 0;
            self.view.row_offset = 0;
            self.selected_rows.clear();
        }
        self.normalize_sort_column()?;
        Ok(())
    }

    fn previous_sheet(&mut self) -> AppResult<()> {
        let names = self.database.get_table_names()?;
        let target = self
            .previous_sheet
            .clone()
            .filter(|name| names.contains(name))
            .or_else(|| {
                self.sheet_order
                    .iter()
                    .find(|name| names.contains(name))
                    .cloned()
            });
        let Some(name) = target else {
            return Ok(());
        };
        let current = self.view.table_name.clone();
        self.switch_sheet(name)?;
        self.previous_sheet = Some(current);
        Ok(())
    }

    fn move_sheet_order(&mut self, toward_start: bool) -> AppResult<()> {
        let name = self.highlighted_table_name()?;
        let Some(position) = self
            .sheet_order
            .iter()
            .position(|candidate| candidate == &name)
        else {
            return Err(app_error_other!("The highlighted sheet is not registered"));
        };
        let target = if toward_start {
            position.checked_sub(1)
        } else if position + 1 < self.sheet_order.len() {
            Some(position + 1)
        } else {
            None
        };
        if let Some(target) = target {
            self.sheet_order.swap(position, target);
            self.view.row = target;
            self.view.row_offset = 0;
        }
        Ok(())
    }

    fn reconcile_sheet_order(&mut self) -> AppResult<()> {
        let mut names = self.database.get_table_names()?;
        names.retain(|name| name != "table_of_tables");
        self.sheet_order.retain(|name| names.contains(name));
        for name in names {
            if !self.sheet_order.contains(&name) {
                self.sheet_order.push(name);
            }
        }
        Ok(())
    }

    fn refresh_sheet_list(&mut self) -> AppResult<()> {
        self.database.view_metadata_table()?;
        for (table, sources) in &self.table_sources {
            self.database.connection.execute(
                "UPDATE table_of_tables SET source_names = ?1 WHERE table_name = ?2",
                rusqlite::params![sources.join(", "), table],
            )?;
        }
        self.database.invalidate();
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

    fn show_analysis(&mut self, frequency: bool) -> AppResult<()> {
        let before = self.view_snapshot();
        let source = self.view.table_name.clone();
        let result = if frequency {
            let column = self.selected_column()?;
            let (result, membership) = self
                .database
                .frequency_table_with_members(&source, &column)?;
            self.frequency_sources
                .insert(result.clone(), (source.clone(), membership));
            self.frequency_selection_baselines
                .insert(result.clone(), self.selected_rows.clone());
            result
        } else {
            self.database.describe_table(&source)?
        };
        self.table_sources
            .insert(result.clone(), vec![source.clone()]);
        let hidden_name = self.database.internal_table_name()?;
        self.switch_sheet(result.clone())?;
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        self.view.order_column = Some("id".into());
        self.view.ascending = true;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: result,
                hidden_name,
                is_visible: true,
            },
            before,
        );
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
        Ok(())
    }

    fn join_table(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let target = inputs
            .first()
            .ok_or_else(|| app_error_other!("Enter a target as table.column"))?;
        let (right, right_column) = target
            .split_once('.')
            .ok_or_else(|| app_error_other!("Join target must use table.column"))?;
        if right.is_empty() || right_column.is_empty() || right == "table_of_tables" {
            return Err(app_error_other!("Invalid join target"));
        }
        let before = self.view_snapshot();
        let left = self.view.table_name.clone();
        let left_column = self.selected_column()?;
        let result = self
            .database
            .join_tables(&left, &left_column, right, right_column)?;
        self.table_sources
            .insert(result.clone(), vec![left.clone(), right.to_string()]);
        let hidden_name = self.database.internal_table_name()?;
        self.view.table_name = result.clone();
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        self.view.order_column = Some("id".into());
        self.view.ascending = true;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: result,
                hidden_name,
                is_visible: true,
            },
            before,
        );
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
        Ok(())
    }

    fn toggle_join_key(&mut self) -> AppResult<()> {
        if self.view.table_name == "table_of_tables" {
            return Err(app_error_other!("Mark join keys in each source table"));
        }
        let table = self.view.table_name.clone();
        let column = self.selected_column()?;
        let keys = self.join_keys.entry(table).or_default();
        if let Some(index) = keys.iter().position(|key| key == &column) {
            keys.remove(index);
            self.ui.last_command = PreviousCommand::new(
                Command::ToggleJoinKey,
                Some(format!("Unmarked {column} as a join key")),
            );
        } else {
            keys.push(column.clone());
            self.ui.last_command = PreviousCommand::new(
                Command::ToggleJoinKey,
                Some(format!("Marked {column} as join key #{}", keys.len())),
            );
        }
        Ok(())
    }

    fn combine_selected_tables(&mut self, inputs: Vec<String>) -> AppResult<()> {
        if self.view.table_name != "table_of_tables" {
            return Err(app_error_other!(
                "Open the sheet list and select tables first"
            ));
        }
        let tables = self
            .sheet_order
            .iter()
            .filter(|name| self.selected_sheets.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        if tables.len() < 2 {
            return Err(app_error_other!(
                "Select at least two tables in the sheet list"
            ));
        }
        let action = inputs
            .first()
            .map(|value| value.trim().to_ascii_lowercase())
            .ok_or_else(|| app_error_other!("Choose append, inner, left, or full"))?;
        let before = self.view_snapshot();
        let result = if action == "append" {
            self.database.append_tables(&tables)?
        } else {
            if tables.len() != 2 {
                return Err(app_error_other!(
                    "Joins require exactly two selected tables"
                ));
            }
            let left_keys = self.join_keys.get(&tables[0]).cloned().unwrap_or_default();
            let right_keys = self.join_keys.get(&tables[1]).cloned().unwrap_or_default();
            if left_keys.is_empty() || left_keys.len() != right_keys.len() {
                return Err(app_error_other!(
                    "Mark the same number of ordered join keys on both tables"
                ));
            }
            let keys = left_keys.into_iter().zip(right_keys).collect::<Vec<_>>();
            let kind = match action.as_str() {
                "inner" => JoinKind::Inner,
                "left" => JoinKind::Left,
                "full" => JoinKind::Full,
                _ => return Err(app_error_other!("Choose append, inner, left, or full")),
            };
            self.database
                .join_tables_with_keys(&tables[0], &tables[1], &keys, kind)?
        };
        self.table_sources.insert(result.clone(), tables.clone());
        let hidden_name = self.database.internal_table_name()?;
        self.selected_sheets.clear();
        self.switch_sheet(result.clone())?;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: result,
                hidden_name,
                is_visible: true,
            },
            before,
        );
        self.ui.last_command = PreviousCommand::new(
            Command::CombineTables,
            Some(format!("Created result from {} tables", tables.len())),
        );
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
        Ok(())
    }

    fn configure_aggregator(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let column = self.selected_column()?;
        let label = inputs
            .first()
            .map(|value| value.trim().to_ascii_lowercase())
            .ok_or_else(|| app_error_other!("Choose count, distinct, sum, average, min, or max"))?;
        let aggregator = match label.as_str() {
            "count" => Aggregator::Count,
            "distinct" | "distinct_count" => Aggregator::DistinctCount,
            "sum" => Aggregator::Sum,
            "avg" | "average" => Aggregator::Average,
            "min" => Aggregator::Min,
            "max" => Aggregator::Max,
            _ => {
                return Err(app_error_other!(
                    "Choose count, distinct, sum, average, min, or max"
                ))
            }
        };
        let table = self.view.table_name.clone();
        let columns = self.aggregators.entry(table).or_default();
        if let Some(existing) = columns.iter_mut().find(|(name, _)| name == &column) {
            existing.1 = aggregator;
        } else {
            columns.push((column.clone(), aggregator));
        }
        self.ui.last_command = PreviousCommand::new(
            Command::ConfigureAggregator,
            Some(format!("{} configured for {column}", aggregator.name())),
        );
        Ok(())
    }

    fn create_grouped_sheet(&mut self) -> AppResult<()> {
        let source = self.view.table_name.clone();
        let keys = self.join_keys.get(&source).cloned().unwrap_or_default();
        let aggregations = self.aggregators.get(&source).cloned().unwrap_or_default();
        let before = self.view_snapshot();
        let result = self.database.grouped_table(&source, &keys, &aggregations)?;
        self.table_sources
            .insert(result.clone(), vec![source.clone()]);
        self.open_created_result(result, source, before, Command::Group)
    }

    fn create_pivot_sheet(&mut self) -> AppResult<()> {
        let source = self.view.table_name.clone();
        let pivot_column = self.selected_column()?;
        let keys = self.join_keys.get(&source).cloned().unwrap_or_default();
        if keys.contains(&pivot_column) {
            return Err(app_error_other!(
                "The pivot column cannot also be a row key"
            ));
        }
        let aggregations = self.aggregators.get(&source).cloned().unwrap_or_default();
        let before = self.view_snapshot();
        let result = self
            .database
            .pivot_table(&source, &keys, &pivot_column, &aggregations)?;
        self.table_sources
            .insert(result.clone(), vec![source.clone()]);
        self.open_created_result(result, source, before, Command::Pivot)
    }

    fn open_created_result(
        &mut self,
        result: String,
        source: String,
        before: ViewSnapshot,
        command: Command,
    ) -> AppResult<()> {
        let hidden_name = self.database.internal_table_name()?;
        self.switch_sheet(result.clone())?;
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        self.view.order_column = Some("id".into());
        self.view.ascending = true;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: result.clone(),
                hidden_name,
                is_visible: true,
            },
            before,
        );
        self.ui.last_command =
            PreviousCommand::new(command, Some(format!("Created result from {source}")));
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
        Ok(())
    }

    fn toggle_selection(&mut self) -> AppResult<()> {
        if self.view.table_name == "table_of_tables" {
            let name = self.highlighted_table_name()?;
            if !self.selected_sheets.insert(name.clone()) {
                self.selected_sheets.remove(&name);
            }
            self.ui.last_command = PreviousCommand::new(
                Command::ToggleSelection,
                Some(format!("{} tables selected", self.selected_sheets.len())),
            );
            return Ok(());
        }
        let id = self
            .view
            .rows
            .get(self.view.row)
            .and_then(|row| row.get(*self.view.column_indexes.first()?))
            .ok_or_else(|| app_error_other!("No selected row"))?
            .to_string()
            .parse::<i64>()
            .map_err(|err| app_error_other!(err.to_string()))?;
        if !self.selected_rows.insert(id) {
            self.selected_rows.remove(&id);
        }
        self.sync_frequency_source_selection()?;
        self.ui.last_command = PreviousCommand::new(
            Command::ToggleSelection,
            Some(format!("{} rows selected", self.selected_rows.len())),
        );
        Ok(())
    }

    fn clear_selection(&mut self) -> AppResult<()> {
        if self.view.table_name == "table_of_tables" {
            self.selected_sheets.clear();
            self.ui.last_command = PreviousCommand::new(
                Command::ClearSelection,
                Some("Table selection cleared".into()),
            );
            return Ok(());
        }
        self.selected_rows.clear();
        self.sync_frequency_source_selection()?;
        self.ui.last_command =
            PreviousCommand::new(Command::ClearSelection, Some("Selection cleared".into()));
        Ok(())
    }

    fn sync_frequency_source_selection(&mut self) -> AppResult<()> {
        let Some((source, membership)) = self.frequency_sources.get(&self.view.table_name).cloned()
        else {
            return Ok(());
        };
        let mut selected = self
            .frequency_selection_baselines
            .get(&self.view.table_name)
            .cloned()
            .unwrap_or_default();
        for group_id in &*self.selected_rows {
            selected.extend(
                self.database
                    .frequency_group_members(&membership, *group_id)?,
            );
        }
        if let Some(source_view) = self.sheet_views.get_mut(&source) {
            source_view.selection = selected;
        }
        Ok(())
    }

    fn drill_into_frequency_group(&mut self) -> AppResult<()> {
        let current = self.view.table_name.clone();
        let Some((source, membership)) = self.frequency_sources.get(&current).cloned() else {
            return Ok(());
        };
        let row = self
            .view
            .rows
            .get(self.view.row)
            .ok_or_else(|| app_error_other!("No frequency group is highlighted"))?;
        let group_id = row
            .get(
                *self
                    .view
                    .column_indexes
                    .first()
                    .ok_or_else(|| app_error_other!("Frequency ID column is hidden"))?,
            )
            .ok_or_else(|| app_error_other!("Frequency ID is unavailable"))?
            .to_string()
            .parse::<i64>()
            .map_err(|error| app_error_other!(error.to_string()))?;
        let members = self
            .database
            .frequency_group_members(&membership, group_id)?;
        let mut selection = self
            .frequency_selection_baselines
            .get(&current)
            .cloned()
            .unwrap_or_default();
        selection.extend(members);
        if let Some(source_view) = self.sheet_views.get_mut(&source) {
            source_view.selection = selection;
        }
        self.switch_sheet(source)?;
        self.drilldown_return_sheet = Some(current);
        self.ui.last_command = PreviousCommand::new(
            Command::HistoryReplay,
            Some("Source rows highlighted; Esc returns to frequencies".into()),
        );
        Ok(())
    }

    fn leave_frequency_drilldown(&mut self) -> AppResult<()> {
        let Some(target) = self.drilldown_return_sheet.take() else {
            return self.previous_sheet();
        };
        self.switch_sheet(target)?;
        Ok(())
    }
    fn select_sql(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let condition = inputs
            .first()
            .ok_or_else(|| app_error_other!("Enter a SQL condition"))?;
        let table = self.view.table_name.replace('"', "\"\"");
        let mut stmt = self
            .database
            .connection
            .prepare(&format!("SELECT id FROM \"{table}\" WHERE {condition}"))?;
        let ids = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        self.selected_rows.extend(ids);
        self.ui.last_command = PreviousCommand::new(
            Command::SelectSql,
            Some(format!("{} rows selected", self.selected_rows.len())),
        );
        Ok(())
    }
    fn select_regex(&mut self, inputs: Vec<String>) -> AppResult<()> {
        let pattern = inputs
            .first()
            .ok_or_else(|| app_error_other!("Enter a regex"))?;
        let column = self.selected_column()?;
        let ids = self
            .database
            .select_regex(&self.view.table_name, &column, pattern)?;
        self.selected_rows.extend(ids);
        self.ui.last_command = PreviousCommand::new(
            Command::SelectRegex,
            Some(format!("{} rows selected", self.selected_rows.len())),
        );
        Ok(())
    }
    fn delete_selected(&mut self) -> AppResult<()> {
        if self.selected_rows.is_empty() {
            return Err(app_error_other!("No rows selected"));
        }
        let table = self.view.table_name.clone();
        let before = self.view_snapshot();
        let record = self.database.delete_selected(&table, &self.selected_rows)?;
        self.record_history(record, before);
        self.selected_rows.clear();
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
        self.ui.last_command = PreviousCommand::new(
            Command::DeleteSelected,
            Some("Deleted selected rows".into()),
        );
        Ok(())
    }
    fn copy_selected(&mut self) -> AppResult<()> {
        if self.selected_rows.is_empty() {
            return Err(app_error_other!("No rows selected"));
        }
        let source = self.view.table_name.clone();
        let mut target = format!("{source}_selected");
        let names = self.database.get_table_names()?;
        let base = target.clone();
        let mut n = 2;
        while names.contains(&target) {
            target = format!("{base}_{n}");
            n += 1;
        }
        self.database
            .copy_selected(&source, &target, &self.ordering(), &self.selected_rows)?;
        let hidden = self.database.internal_table_name()?;
        let before = self.view_snapshot();
        self.view.table_name = target.clone();
        self.selected_rows.clear();
        self.view.row = 0;
        self.view.row_offset = 0;
        self.view.column = 0;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: target,
                hidden_name: hidden,
                is_visible: true,
            },
            before,
        );
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
        Ok(())
    }

    fn bulk_edit(&mut self, inputs: Vec<String>) -> AppResult<()> {
        if self.selected_rows.is_empty() {
            return Err(app_error_other!(
                "Select rows with Space before bulk editing"
            ));
        }
        let before_view = self.view_snapshot();
        let table = self.view.table_name.clone();
        let column = self.selected_column()?;
        if column == "id" {
            return Err(app_error_other!("The id column is read-only"));
        }
        let value = inputs
            .first()
            .ok_or_else(|| app_error_other!("No value provided"))?;
        let record =
            self.database
                .bulk_edit_selected(&table, &column, &self.selected_rows, value)?;
        self.record_history(record, before_view);
        self.ui.last_command =
            PreviousCommand::new(Command::BulkEdit, Some("Updated selected rows".into()));
        Ok(())
    }

    fn delete_table(&mut self) -> AppResult<()> {
        let before = self.view_snapshot();
        let from_sheet_list = self.view.table_name == "table_of_tables";
        if from_sheet_list {
            let mut tables = self
                .database
                .get_table_names()?
                .into_iter()
                .filter(|name| self.selected_sheets.contains(name))
                .collect::<Vec<_>>();
            if tables.is_empty() {
                tables.push(self.highlighted_table_name()?);
            }
            if tables.iter().any(|table| table == "table_of_tables") {
                return Err(app_error_other!("The metadata table cannot be deleted"));
            }
            let hidden = self.database.delete_tables(&tables)?;
            let records = hidden
                .into_iter()
                .map(|(visible_name, hidden_name)| UndoRecord::Visibility {
                    table: None,
                    visible_name,
                    hidden_name,
                    is_visible: false,
                })
                .collect();
            self.selected_sheets.clear();
            self.refresh_sheet_list()?;
            self.record_history(UndoRecord::Group(records), before);
            self.ui.last_command = PreviousCommand::new(
                Command::DeleteTable,
                Some(format!(
                    "Deleted {} table(s); Ctrl+Z restores them",
                    tables.len()
                )),
            );
            return Ok(());
        }
        let table = self.view.table_name.clone();
        if table == "table_of_tables" {
            return Err(app_error_other!("The metadata table cannot be deleted"));
        }
        let hidden_name = self.database.delete_table(&table)?;
        self.change_table(false)?;
        self.record_history(
            UndoRecord::Visibility {
                table: None,
                visible_name: table.clone(),
                hidden_name,
                is_visible: false,
            },
            before,
        );
        self.ui.last_command = PreviousCommand::new(
            Command::DeleteTable,
            Some(format!("Deleted {table}; Ctrl+Z restores it")),
        );
        Ok(())
    }

    fn highlighted_table_name(&self) -> AppResult<String> {
        if self.view.table_name != "table_of_tables" {
            return Err(app_error_other!("The sheet list is not open"));
        }
        let row = self.view.row.saturating_sub(self.view.row_offset as usize);
        let display_column = self
            .view
            .headers
            .iter()
            .position(|header| header == "table_name")
            .ok_or_else(|| app_error_other!("Sheet list has no table_name column"))?;
        let raw_column = *self
            .view
            .column_indexes
            .get(display_column)
            .ok_or_else(|| app_error_other!("Sheet list layout is out of date"))?;
        match self
            .view
            .rows
            .get(row)
            .and_then(|values| values.get(raw_column))
        {
            Some(DataItem::Text(name)) => Ok(name.clone()),
            Some(value) => Ok(value.to_string()),
            None => Err(app_error_other!("No table is highlighted")),
        }
    }

    fn open_highlighted_table(&mut self) -> AppResult<()> {
        let table = self.highlighted_table_name()?;
        self.switch_sheet(table)
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
        self.refresh_sheet_list()?;
        self.reconcile_sheet_order()?;
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
            let needs_transform_template = queued_command.command == Command::RegexTransform
                && queued_command.inputs.is_empty()
                && regex::Regex::new(&self.ui.input)
                    .is_ok_and(|regex| regex.capture_names().len() > 1);
            if !needs_transform_template {
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

    pub(super) fn dispatch(&mut self, command: Command, height: u32) -> AppResult<()> {
        if self.ui.history_view {
            match command {
                Command::History | Command::HistoryClose => self.ui.history_view = false,
                Command::Move(Direction::Up) => {
                    self.ui.history_cursor = self.ui.history_cursor.saturating_sub(1)
                }
                Command::Move(Direction::Down) => {
                    self.ui.history_cursor = (self.ui.history_cursor + 1)
                        .min(self.command_history.len().saturating_sub(1))
                }
                Command::HistoryReplay => {
                    if let Err(err) = self.replay_history() {
                        self.ui.last_command =
                            PreviousCommand::new(Command::IllegalOperation, Some(err.to_string()));
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if self.drilldown_return_sheet.is_some()
            && matches!(
                &command,
                Command::Edit
                    | Command::RegexTransform
                    | Command::RegexFilter
                    | Command::SqlQuery
                    | Command::TextToInt
                    | Command::IntToText
                    | Command::DeleteColumn
                    | Command::DeleteTable
                    | Command::MathOperation
                    | Command::RenameTable
                    | Command::RenameColumn
                    | Command::BulkEdit
                    | Command::DeleteSelected
                    | Command::Copy
                    | Command::Undo
                    | Command::Redo
                    | Command::HistoryReplay
                    | Command::ConfigureAggregator
                    | Command::Group
                    | Command::Pivot
            )
        {
            return Err(app_error_other!("Frequency source rows are read-only"));
        }
        let before_command = self.view_snapshot();
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
            | Command::RenameTable
            | Command::Join
            | Command::CombineTables
            | Command::ConfigureAggregator
            | Command::BulkEdit => {
                self.ui.queued_command = Some(QueuedCommand::new(command.clone()));
                self.ui
                    .input_mode_state_machine
                    .transition(input::Event::StartEditing)?;
                Ok(())
            }
            Command::SelectSql | Command::SelectRegex => {
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
            Command::Move(direction) => self.move_cursor(direction, height),
            Command::NextTable => self.change_table(true),
            Command::PrevTable => self.change_table(false),
            Command::PreviousSheet if self.drilldown_return_sheet.is_some() => {
                self.leave_frequency_drilldown()
            }
            Command::PreviousSheet => self.previous_sheet(),
            Command::TextToInt => self.text_to_int(),
            Command::IntToText => self.int_to_text(),
            Command::DeleteColumn => self.delete_column(),
            Command::ToggleJoinKey => self.toggle_join_key(),
            Command::Group => self.create_grouped_sheet(),
            Command::Pivot => self.create_pivot_sheet(),
            Command::DeleteTable => self.delete_table(),
            Command::ToggleMetadataTable => self.toggle_metadata_table(),
            Command::HistoryReplay if self.view.table_name == "table_of_tables" => {
                self.open_highlighted_table()
            }
            Command::HistoryReplay
                if self.frequency_sources.contains_key(&self.view.table_name) =>
            {
                self.drill_into_frequency_group()
            }
            Command::Undo | Command::Redo => self.dispatch_history_command(command.clone()), // Command::Join(_) => todo!(),
            Command::Summary => self.show_analysis(false),
            Command::Frequency => self.show_analysis(true),
            Command::ToggleSelection => self.toggle_selection(),
            Command::ClearSelection => self.clear_selection(),
            Command::DeleteSelected if self.view.table_name == "table_of_tables" => {
                self.delete_table()
            }
            Command::DeleteSelected => self.delete_selected(),
            Command::CopySelected => self.copy_selected(),
            Command::HideColumn => self.hide_selected_column(),
            Command::ShowColumns => {
                self.show_all_columns();
                Ok(())
            }
            Command::ColumnLeft if self.view.table_name == "table_of_tables" => {
                self.move_sheet_order(true)
            }
            Command::ColumnRight if self.view.table_name == "table_of_tables" => {
                self.move_sheet_order(false)
            }
            Command::ColumnLeft => self.move_column(true),
            Command::ColumnRight => self.move_column(false),
            Command::History => {
                self.ui.history_view = true;
                self.ui.history_cursor = self.command_history.len().saturating_sub(1);
                Ok(())
            }
            Command::HistoryClose if self.drilldown_return_sheet.is_some() => {
                self.leave_frequency_drilldown()
            }
            Command::HistoryClose if self.view.table_name == "table_of_tables" => {
                self.previous_sheet()
            }
            Command::HistoryReplay | Command::HistoryClose => Ok(()),
        };
        match command {
            Command::RenameTable => {
                let old_table_name = if self.view.table_name == "table_of_tables" {
                    self.highlighted_table_name().unwrap_or_default()
                } else {
                    self.view.table_name.clone()
                };
                self.ui.character_index = old_table_name.len();
                self.ui.input = old_table_name;
            }

            Command::RenameColumn => {
                log::info!("testing 2");
            }
            Command::CombineTables => {
                self.ui.last_command.message = Some("Type append, inner, left, or full".into());
            }
            Command::ConfigureAggregator => {
                self.ui.last_command.message =
                    Some("Type count, distinct, sum, average, min, or max".into());
            }
            _ => {}
        }

        let prompted = matches!(
            command,
            Command::RegexTransform
                | Command::Save
                | Command::RegexFilter
                | Command::Edit
                | Command::ExactSearch
                | Command::SqlQuery
                | Command::RenameColumn
                | Command::MathOperation
                | Command::RenameTable
                | Command::Join
                | Command::CombineTables
                | Command::ConfigureAggregator
                | Command::BulkEdit
                | Command::SelectSql
                | Command::SelectRegex
        );
        if result.is_ok() && !prompted {
            self.log_command(command.clone(), Vec::new(), before_command);
        }

        if let Err(err) = &result {
            log::info!("Error: {:?}", err);
        }
        result
    }
    /// Execute a command synchronously for programmatic callers. The terminal
    /// uses the worker protocol instead, so this never runs on its UI thread.
    pub fn execute(&mut self, command: Command, inputs: Vec<String>) -> AppResult<()> {
        self.refresh_page(100)?;
        self.ui.last_command = PreviousCommand::new(command.clone(), None);
        if super::worker::prompted(&command) {
            self.execute_inputs(command, inputs)?;
        } else {
            self.dispatch(command, self.table_height)?;
        }
        self.refresh_page(100)?;
        if self.ui.last_command.command == Command::IllegalOperation {
            return Err(AppError::from(
                self.ui
                    .last_command
                    .message
                    .as_deref()
                    .unwrap_or("Command failed"),
            ));
        }
        Ok(())
    }

    pub fn run(self, tui: TUI) -> AppResult<()> {
        super::worker::run(
            move |token| {
                let mut controller = self;
                controller.database.cancellation = token.clone();
                Database::install_cancellation(&controller.database.connection, token);
                Ok(controller)
            },
            tui,
        )
    }

    pub fn run_paths(paths: Vec<PathBuf>, tui: TUI) -> AppResult<()> {
        super::worker::run(
            move |token| Database::load_cancellable(paths, token).map(Self::new),
            tui,
        )
    }

    pub(super) fn execute_inputs(
        &mut self,
        command: Command,
        inputs: Vec<String>,
    ) -> AppResult<()> {
        if inputs.is_empty() {
            return Err(AppError::from("Command input is required"));
        }
        let before_command = self.view_snapshot();
        self.ui.queued_command = Some(QueuedCommand {
            command: command.clone(),
            inputs: inputs.clone(),
        });
        self.execute_queued_command()?;
        self.ui.queued_command = None;
        if self.ui.last_command.command != Command::IllegalOperation {
            if command == Command::SqlQuery {
                self.ui.last_command = PreviousCommand::new(
                    command,
                    Some("SQL executed; undo history was cleared (SQL is not undoable)".into()),
                );
            } else {
                self.log_command(command.clone(), inputs, before_command);
                self.ui.last_command = PreviousCommand::new(command, Some("Completed".into()));
            }
        }
        Ok(())
    }

    pub(super) fn shutdown(&mut self) {
        self.clear_history();
    }

    pub fn get_headers_and_rows(&mut self, limit: u32) -> AppResult<DataTable> {
        self.refresh_page(limit)?;
        let rows = self
            .view
            .rows
            .iter()
            .map(|row| {
                self.view
                    .column_indexes
                    .iter()
                    .filter_map(|index| row.get(*index).cloned())
                    .collect()
            })
            .collect();
        Ok((self.view.headers.clone(), rows))
    }

    pub(super) fn refresh_page(&mut self, limit: u32) -> AppResult<()> {
        let ordering = self.ordering();
        let layout = self
            .column_layouts
            .get(&self.view.table_name)
            .cloned()
            .unwrap_or_default();
        let key = (
            self.view.table_name.clone(),
            ordering.clone(),
            self.view.row_offset,
            limit,
            self.database.revision(),
            layout,
        );
        if self.page_key.as_ref() == Some(&key) {
            return Ok(());
        }
        let source_key = (
            self.view.table_name.clone(),
            ordering.clone(),
            self.view.row_offset,
            limit,
            self.database.revision(),
        );
        if self.source_page.as_ref().map(|page| &page.key) != Some(&source_key) {
            let (headers, rows) = self.database.get(
                limit,
                self.view.row_offset,
                &self.view.table_name,
                &ordering,
            )?;
            self.source_page = Some(SourcePage {
                key: source_key,
                headers,
                rows: Arc::new(rows),
            });
        }
        let page = self.source_page.as_ref().unwrap();
        let layout = self
            .column_layouts
            .entry(self.view.table_name.clone())
            .or_default();
        let positions = page
            .headers
            .iter()
            .enumerate()
            .map(|(index, header)| (header.as_str(), index))
            .collect::<HashMap<_, _>>();
        let known = layout
            .order
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let missing = page
            .headers
            .iter()
            .filter(|header| !known.contains(header.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        layout.order.extend(missing);
        layout
            .order
            .retain(|header| positions.contains_key(header.as_str()));
        if let Some(id_index) = layout.order.iter().position(|header| header == "id") {
            if id_index != 0 {
                layout.order.swap(0, id_index);
            }
        }
        layout.hidden.remove("id");
        let indexes = layout
            .order
            .iter()
            .filter(|header| !layout.hidden.contains(*header))
            .filter_map(|header| positions.get(header.as_str()).copied())
            .collect::<Vec<_>>();
        self.view.headers = indexes
            .iter()
            .map(|index| page.headers[*index].clone())
            .collect();
        self.view.rows = page.rows.clone();
        self.view.column_indexes = indexes;
        self.view.row = self.view.row.min(self.view.rows.len().saturating_sub(1));
        self.view.column = self
            .view
            .column
            .min(self.view.headers.len().saturating_sub(1));
        self.page_key = Some((
            self.view.table_name.clone(),
            ordering,
            self.view.row_offset,
            limit,
            self.database.revision(),
            layout.clone(),
        ));
        Ok(())
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
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
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
            .and_then(|r| r.get(*self.view.column_indexes.first()?))
            .map(|v| v.to_string())
            .ok_or_else(|| app_error_other!("No selected row"))?
            .parse::<i64>()
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
                let height = self.table_height.max(1);
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
        if let Some(keys) = self.join_keys.get_mut(&table) {
            keys.retain(|key| key != &column);
        }
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
        if let Some(keys) = self.join_keys.get_mut(&table) {
            for key in keys {
                if key == &old_column {
                    *key = new_column.clone();
                }
            }
        }
        let layout = self.column_layouts.entry(table.clone()).or_default();
        for header in &mut layout.order {
            if header == &old_column {
                *header = new_column.clone();
            }
        }
        if layout.hidden.remove(&old_column) {
            layout.hidden.insert(new_column.clone());
        }
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
        if !self.defer_refresh {
            self.refresh_page(100)?;
        }
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
                    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
                        "csv" => savers::csv(&self.database, &self.view.table_name, &path),
                        "tsv" => savers::tsv(&self.database, &self.view.table_name, &path),
                        "sqlite" | "db" | "sql" | "sqlite3" => {
                            savers::sqlite(&self.database, &path)
                        }
                        _ => Err(AppError::from("Unsupported file format")),
                    }
                }
                Command::MathOperation => self.math_operation(inputs),
                Command::Join => self.join_table(inputs),
                Command::CombineTables => self.combine_selected_tables(inputs),
                Command::ConfigureAggregator => self.configure_aggregator(inputs),
                Command::BulkEdit => self.bulk_edit(inputs),
                Command::SelectSql => self.select_sql(inputs),
                Command::SelectRegex => self.select_regex(inputs),
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
                | Command::PreviousSheet
                | Command::TextToInt
                | Command::IntToText
                | Command::DeleteTable
                | Command::Undo
                | Command::Redo
                | Command::ToggleMetadataTable
                | Command::DeleteColumn
                | Command::Summary
                | Command::Frequency
                | Command::ToggleSelection
                | Command::ToggleJoinKey
                | Command::Group
                | Command::Pivot
                | Command::DeleteSelected
                | Command::CopySelected
                | Command::ClearSelection
                | Command::HideColumn
                | Command::ShowColumns
                | Command::ColumnLeft
                | Command::ColumnRight
                | Command::History
                | Command::HistoryReplay
                | Command::HistoryClose => {
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
        let from_sheet_list = self.view.table_name == "table_of_tables";
        let old_table_name = if from_sheet_list {
            self.highlighted_table_name()?
        } else {
            self.view.table_name.clone()
        };
        if old_table_name == "table_of_tables" {
            return Err(app_error_other!("The metadata table cannot be renamed"));
        }
        if new_table_name.starts_with(INTERNAL_PREFIX) {
            return Err(app_error_other!(
                "Table names using Dataman's reserved undo prefix are not supported"
            ));
        }
        self.database
            .rename_table(&old_table_name, &new_table_name)?;
        let renamed_table_name = new_table_name.clone();
        if let Some(layout) = self.column_layouts.remove(&old_table_name) {
            self.column_layouts.insert(new_table_name.clone(), layout);
        }
        if let Some(sheet_view) = self.sheet_views.remove(&old_table_name) {
            self.sheet_views.insert(new_table_name.clone(), sheet_view);
        }
        if self.previous_sheet.as_deref() == Some(&old_table_name) {
            self.previous_sheet = Some(new_table_name.clone());
        }
        if let Some(position) = self
            .sheet_order
            .iter()
            .position(|name| name == &old_table_name)
        {
            self.sheet_order[position] = new_table_name.clone();
        }
        if self.selected_sheets.remove(&old_table_name) {
            self.selected_sheets.insert(new_table_name.clone());
        }
        if let Some(sources) = self.table_sources.remove(&old_table_name) {
            self.table_sources.insert(new_table_name.clone(), sources);
        }
        for sources in self.table_sources.values_mut() {
            for source in sources {
                if source == &old_table_name {
                    *source = new_table_name.clone();
                }
            }
        }
        if from_sheet_list {
            self.refresh_sheet_list()?;
        } else {
            self.view.table_name = new_table_name;
        }
        self.ui.last_command = PreviousCommand::new(Command::RenameTable, None);
        self.record_history(
            UndoRecord::RenameTable {
                before: old_table_name,
                after: if from_sheet_list {
                    renamed_table_name
                } else {
                    self.view.table_name.clone()
                },
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
    fn switching_sheets_restores_cursor_sort_and_row_selection() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("CREATE TABLE other (id INTEGER PRIMARY KEY, value TEXT);")
            .unwrap();
        controller.view.row = 1;
        controller.view.row_offset = 1;
        controller.view.order_column = Some("name".into());
        controller.view.ascending = false;
        controller.selected_rows.insert(2);

        controller.switch_sheet("other".into()).unwrap();
        assert_eq!(controller.view.table_name, "other");
        assert_eq!(controller.view.row, 0);
        assert!(controller.selected_rows.is_empty());

        controller.view.row = 3;
        controller.selected_rows.insert(9);
        controller.switch_sheet("t".into()).unwrap();
        assert_eq!(controller.view.row, 1);
        assert_eq!(controller.view.row_offset, 1);
        assert_eq!(controller.view.order_column.as_deref(), Some("name"));
        assert!(!controller.view.ascending);
        assert!(controller.selected_rows.contains(&2));

        controller.previous_sheet().unwrap();
        assert_eq!(controller.view.table_name, "other");
        assert_eq!(controller.view.row, 3);
        assert!(controller.selected_rows.contains(&9));
    }

    #[test]
    fn sheet_list_opens_highlighted_table_and_deletion_undo_restores_it() {
        let mut controller = controller();
        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        assert_eq!(controller.highlighted_table_name().unwrap(), "t");
        controller.open_highlighted_table().unwrap();
        assert_eq!(controller.view.table_name, "t");

        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        controller.delete_table().unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        assert_eq!(controller.view.table_name, "table_of_tables");

        controller.undo().unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        controller.refresh_page(20).unwrap();
        assert_eq!(controller.highlighted_table_name().unwrap(), "t");
    }

    #[test]
    fn sheet_list_renames_highlighted_table_and_undoes() {
        let mut controller = controller();
        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        controller.rename_table(vec!["renamed".into()]).unwrap();
        assert_eq!(controller.view.table_name, "table_of_tables");
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"renamed".into()));
        controller.undo().unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        controller.refresh_page(20).unwrap();
        assert_eq!(controller.highlighted_table_name().unwrap(), "t");
    }

    #[test]
    fn selected_tables_delete_as_one_undoable_action() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("CREATE TABLE other (id INTEGER PRIMARY KEY, value TEXT);")
            .unwrap();
        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        controller.view.row = 0;
        controller.toggle_selection().unwrap();
        controller.view.row = 1;
        controller.toggle_selection().unwrap();
        assert_eq!(controller.selected_sheets.len(), 2);

        controller.delete_table().unwrap();
        let remaining = controller.database.get_table_names().unwrap();
        assert_eq!(remaining, vec!["table_of_tables"]);
        assert!(controller.selected_sheets.is_empty());
        controller.undo().unwrap();
        let restored = controller.database.get_table_names().unwrap();
        assert!(restored.contains(&"t".into()));
        assert!(restored.contains(&"other".into()));
        assert_eq!(controller.selected_sheets.len(), 2);
        controller.redo().unwrap();
        assert_eq!(
            controller.database.get_table_names().unwrap(),
            vec!["table_of_tables"]
        );
    }

    #[test]
    fn sheet_list_reordering_changes_navigation_order_without_losing_highlight() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("CREATE TABLE other (id INTEGER PRIMARY KEY, value TEXT);")
            .unwrap();
        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        let highlighted = controller.highlighted_table_name().unwrap();
        controller.move_sheet_order(false).unwrap();
        controller.refresh_page(20).unwrap();
        assert_eq!(controller.highlighted_table_name().unwrap(), highlighted);
        controller.view.row = 0;
        controller.refresh_page(20).unwrap();
        assert_eq!(controller.highlighted_table_name().unwrap(), "other");
        controller.view.table_name = "t".into();
        controller.change_table(true).unwrap();
        assert_eq!(controller.view.table_name, "other");
    }

    #[test]
    fn combine_selected_tables_appends_in_navigation_order_and_undoes() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("CREATE TABLE other (id INTEGER PRIMARY KEY, extra TEXT); INSERT INTO other(extra) VALUES ('x'), ('y');")
            .unwrap();
        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        for name in ["t", "other"] {
            let row = controller
                .view
                .rows
                .iter()
                .position(|values| values[0] == DataItem::Text(name.into()))
                .unwrap();
            controller.view.row = row;
            controller.toggle_selection().unwrap();
        }
        controller
            .combine_selected_tables(vec!["append".into()])
            .unwrap();
        let result = controller.view.table_name.clone();
        assert_eq!(controller.view.table_name, result);
        assert_eq!(controller.database.count_rows(&result), Some(4));
        assert_eq!(
            controller.database.get_headers(&result).unwrap(),
            vec!["id", "name", "extra"]
        );
        controller.toggle_metadata_table().unwrap();
        let sources: String = controller
            .database
            .connection
            .query_row(
                "SELECT source_names FROM table_of_tables WHERE table_name = ?1",
                [&result],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sources, "t, other");
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&result));
        controller.redo().unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&result));
    }

    #[test]
    fn marked_composite_keys_join_selected_tables_through_controller() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("CREATE TABLE lefts (id INTEGER PRIMARY KEY, key TEXT, value TEXT); INSERT INTO lefts(key,value) VALUES ('x','l1'), ('y','l2'); CREATE TABLE rights (id INTEGER PRIMARY KEY, key TEXT, value TEXT); INSERT INTO rights(key,value) VALUES ('x','r1'), ('z','r2');")
            .unwrap();
        controller.toggle_metadata_table().unwrap();
        controller.refresh_page(20).unwrap();
        for name in ["lefts", "rights"] {
            let row = controller
                .view
                .rows
                .iter()
                .position(|values| values[0] == DataItem::Text(name.into()))
                .unwrap();
            controller.view.row = row;
            controller.toggle_selection().unwrap();
            controller.switch_sheet(name.into()).unwrap();
            controller.refresh_page(20).unwrap();
            controller.view.column = 1;
            controller.toggle_join_key().unwrap();
            controller.switch_sheet("table_of_tables".into()).unwrap();
            controller.refresh_page(20).unwrap();
        }
        controller
            .combine_selected_tables(vec!["full".into()])
            .unwrap();
        assert_eq!(
            controller.database.count_rows(&controller.view.table_name),
            Some(3)
        );
        assert!(controller
            .database
            .get_headers(&controller.view.table_name)
            .unwrap()
            .contains(&"lefts_value".into()));
        assert!(controller
            .database
            .get_headers(&controller.view.table_name)
            .unwrap()
            .contains(&"rights_value".into()));
    }

    #[test]
    fn controller_creates_grouped_and_pivoted_sheets_from_key_and_aggregate_markers() {
        let mut controller = controller();
        controller.database.execute_batch("ALTER TABLE t ADD COLUMN customer TEXT; ALTER TABLE t ADD COLUMN month TEXT; ALTER TABLE t ADD COLUMN amount INTEGER; UPDATE t SET customer='A', month=CASE id WHEN 1 THEN 'Jan' ELSE 'Feb' END, amount=CASE id WHEN 1 THEN 10 ELSE 20 END;").unwrap();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = controller
            .view
            .headers
            .iter()
            .position(|header| header == "customer")
            .unwrap();
        controller.toggle_join_key().unwrap();
        controller.view.column = controller
            .view
            .headers
            .iter()
            .position(|header| header == "amount")
            .unwrap();
        controller.configure_aggregator(vec!["sum".into()]).unwrap();

        controller.create_grouped_sheet().unwrap();
        let grouped = controller.view.table_name.clone();
        let group_row = controller
            .database
            .get(10, 0, &grouped, "")
            .unwrap()
            .1
            .remove(0);
        assert_eq!(group_row[1], DataItem::Text("A".into()));
        assert_eq!(group_row[2], DataItem::Integer(30));

        controller.switch_sheet("t".into()).unwrap();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = controller
            .view
            .headers
            .iter()
            .position(|header| header == "month")
            .unwrap();
        controller.create_pivot_sheet().unwrap();
        let headers = controller
            .database
            .get_headers(&controller.view.table_name)
            .unwrap();
        assert!(headers.contains(&"sum_amount_Jan".into()));
        assert!(headers.contains(&"sum_amount_Feb".into()));
        controller.undo().unwrap();
        assert_eq!(controller.view.table_name, "t");
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
    fn bulk_edit_undoes_and_redoes_selected_rows() {
        let mut controller = controller();
        controller.selected_rows.extend([1, 2]);
        controller.bulk_edit(vec!["same".into()]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "same"
        );
        assert_eq!(
            controller.database.get_cell("t", 2, "name").unwrap(),
            "same"
        );
        controller.undo().unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        assert_eq!(controller.database.get_cell("t", 2, "name").unwrap(), "def");
        controller.redo().unwrap();
        assert_eq!(
            controller.database.get_cell("t", 2, "name").unwrap(),
            "same"
        );
    }

    #[test]
    fn column_layout_hides_restores_and_reorders_columns() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("ALTER TABLE t ADD COLUMN city TEXT;")
            .unwrap();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        controller.move_column(false).unwrap();
        controller.get_headers_and_rows(10).unwrap();
        assert_eq!(controller.view.headers, vec!["id", "city", "name"]);
        assert_eq!(controller.selected_column().unwrap(), "name");
        controller.hide_selected_column().unwrap();
        controller.get_headers_and_rows(10).unwrap();
        assert_eq!(controller.view.headers, vec!["id", "city"]);
        controller.show_all_columns();
        controller.get_headers_and_rows(10).unwrap();
        assert_eq!(controller.view.headers, vec!["id", "city", "name"]);
    }

    #[test]
    fn logged_command_can_be_replayed_after_undo() {
        let mut controller = controller();
        let before = controller.view_snapshot();
        controller.copy().unwrap();
        controller.log_command(Command::Copy, vec![], before);
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
        controller.replay_history().unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
    }

    #[test]
    fn regex_transform_only_prompts_again_for_capture_templates() {
        let mut controller = controller();
        controller
            .ui
            .input_mode_state_machine
            .transition(input::Event::StartEditing)
            .unwrap();
        controller.ui.queued_command = Some(QueuedCommand::new(Command::RegexTransform));
        controller.ui.input = ".+".into();
        controller.submit_message();
        assert_eq!(
            controller.ui.input_mode_state_machine.get_state(),
            InputMode::Finish
        );

        controller
            .ui
            .input_mode_state_machine
            .transition(input::Event::Reset)
            .unwrap();
        controller
            .ui
            .input_mode_state_machine
            .transition(input::Event::StartEditing)
            .unwrap();
        controller.ui.queued_command = Some(QueuedCommand::new(Command::RegexTransform));
        controller.ui.input = "(.+)".into();
        controller.submit_message();
        assert_eq!(
            controller.ui.input_mode_state_machine.get_state(),
            InputMode::Editing
        );
        assert_eq!(
            controller.ui.queued_command.as_ref().unwrap().inputs,
            vec!["(.+)"]
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
    fn rename_then_delete_uses_the_refreshed_column_name() {
        let mut controller = controller();
        controller.rename_column(vec!["label".into()]).unwrap();
        assert_eq!(controller.selected_column().unwrap(), "label");
        controller.delete_column().unwrap();
        assert!(!controller
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
    fn strict_integer_conversion_keeps_invalid_values_null_through_undo_redo() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("ALTER TABLE t ADD COLUMN raw TEXT; UPDATE t SET raw = CASE id WHEN 1 THEN ' 12 ' ELSE 'bad' END;")
            .unwrap();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 2;
        controller.text_to_int().unwrap();
        assert_eq!(
            controller
                .database
                .get_cell_item("t", 1, "INTEGER_raw")
                .unwrap(),
            DataItem::Integer(12)
        );
        assert_eq!(
            controller
                .database
                .get_cell_item("t", 2, "INTEGER_raw")
                .unwrap(),
            DataItem::Null
        );
        controller.undo().unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_raw".into()));
        controller.redo().unwrap();
        assert_eq!(
            controller
                .database
                .get_cell_item("t", 2, "INTEGER_raw")
                .unwrap(),
            DataItem::Null
        );
    }

    #[test]
    fn summary_command_opens_all_column_describe_view() {
        let mut controller = controller();
        controller.dispatch(Command::Summary, 10).unwrap();
        assert!(controller.view.table_name.ends_with("_describe"));
        assert!(controller.view.headers.contains(&"column_name".into()));
        assert!(controller.view.headers.contains(&"null_count".into()));
        assert!(controller.view.headers.contains(&"distinct_count".into()));
        controller.undo().unwrap();
        assert_eq!(controller.view.table_name, "t");
    }

    #[test]
    fn frequency_group_selection_propagates_to_source_and_clear_restores_baseline() {
        let mut controller = controller();
        controller.selected_rows.insert(2);
        controller.show_analysis(true).unwrap();
        let frequency = controller.view.table_name.clone();
        let row = controller
            .view
            .rows
            .iter()
            .position(|row| row.get(1) == Some(&DataItem::Text("abc".into())))
            .unwrap();
        controller.view.row = row;
        controller.toggle_selection().unwrap();
        assert!(controller.sheet_views["t"].selection.contains(&1));
        assert!(controller.sheet_views["t"].selection.contains(&2));

        controller.clear_selection().unwrap();
        assert!(!controller.sheet_views["t"].selection.contains(&1));
        assert!(controller.sheet_views["t"].selection.contains(&2));
        controller.switch_sheet("t".into()).unwrap();
        assert!(controller.selected_rows.contains(&2));
        assert!(!controller.selected_rows.contains(&1));
        assert_eq!(
            controller.previous_sheet.as_deref(),
            Some(frequency.as_str())
        );
    }

    #[test]
    fn frequency_enter_opens_highlighted_source_rows_read_only_and_escape_returns() {
        let mut controller = controller();
        controller.show_analysis(true).unwrap();
        let frequency = controller.view.table_name.clone();
        let row = controller
            .view
            .rows
            .iter()
            .position(|row| row.get(1) == Some(&DataItem::Text("abc".into())))
            .unwrap();
        controller.view.row = row;
        controller.dispatch(Command::HistoryReplay, 10).unwrap();
        assert_eq!(controller.view.table_name, "t");
        assert!(controller.selected_rows.contains(&1));
        assert!(controller.dispatch(Command::DeleteSelected, 10).is_err());
        assert_eq!(controller.database.count_rows("t"), Some(2));
        controller.dispatch(Command::HistoryClose, 10).unwrap();
        assert_eq!(controller.view.table_name, frequency);
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
        let derived_column = controller.view.headers.len() - 1;
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
    fn id_column_cannot_be_deleted() {
        let mut controller = controller();
        controller.view.column = 0;

        assert!(controller.delete_column().is_err());
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"id".into()));
        assert!(controller.history.undo.is_empty());
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
