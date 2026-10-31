use super::selection::Selection;
use crate::controller::command::{Command, PreviousCommand};
use crate::error::{AppError, AppResult};
use crate::io::savers;
use crate::model::database::{Aggregator, Database, JoinKind, UndoRecord, INTERNAL_PREFIX};
use crate::model::datarow::{DataItem, DataTable};
use crate::navigation::Direction;
use crate::tui::TUI;
#[cfg(test)]
use crossterm::event::{KeyCode, KeyModifiers};
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
};

const HISTORY_LIMIT: usize = 100;

#[derive(Debug)]
struct PreparedCommand {
    action: PreparedAction,
    command: Command,
    inputs: Vec<String>,
}
impl PreparedCommand {
    fn new(command: Command, inputs: Vec<String>, controller: &Controller) -> AppResult<Self> {
        command.validate_inputs(&inputs)?;
        let action = match command {
            Command::HistoryReplay if controller.view.table_name == "table_of_tables" => {
                PreparedAction::OpenSheet
            }
            Command::HistoryReplay
                if controller
                    .sheets
                    .get(&controller.view.table_name)
                    .is_some_and(|sheet| sheet.metadata.frequency.is_some()) =>
            {
                PreparedAction::DrillFrequency
            }
            Command::HistoryClose | Command::PreviousSheet
                if controller.drilldown_return_sheet.is_some() =>
            {
                PreparedAction::LeaveFrequency
            }
            Command::HistoryClose if controller.view.table_name == "table_of_tables" => {
                PreparedAction::PreviousSheet
            }
            _ => PreparedAction::Run,
        };
        Ok(Self {
            action,
            command,
            inputs,
        })
    }
}
#[derive(Debug)]
enum PreparedAction {
    Run,
    OpenSheet,
    DrillFrequency,
    LeaveFrequency,
    PreviousSheet,
}

#[derive(Debug, PartialEq)]
enum ExecutionMode {
    Normal,
    Replay,
}

#[derive(Debug, Clone)]
enum DerivedSpec {
    Describe {
        source: String,
    },
    Frequency {
        source: String,
        column: String,
    },
    Group {
        source: String,
        keys: Vec<String>,
        aggregations: Vec<(String, Aggregator)>,
    },
    Pivot {
        source: String,
        keys: Vec<String>,
        column: String,
        aggregations: Vec<(String, Aggregator)>,
    },
    Append {
        sources: Vec<String>,
    },
    Join {
        left: String,
        right: String,
        keys: Vec<(String, String)>,
        kind: JoinKind,
    },
}

#[derive(Debug)]
pub struct Controller {
    pub(crate) database: Database,
    sheet_list_revision: Option<u64>,
    pub(crate) view: ViewState,
    history: History,
    pub(crate) selected_rows: Selection,
    pub(crate) selected_sheets: HashSet<String>,
    sheets: HashMap<String, SheetState>,
    sheet_order: Vec<String>,
    drilldown_return_sheet: Option<String>,
    previous_sheet: Option<String>,
    page_key: Option<(String, String, u32, u32, u64, ColumnLayout)>,
    source_page: Option<SourcePage>,
    pub(super) table_height: u32,
    pub(crate) command_history: VecDeque<LoggedCommand>,
}

/// Registry entries survive undoable deletion. History projects metadata only;
/// saved navigation and other sheets' layouts keep their established lifetime.
#[derive(Debug, Clone, Default)]
struct SheetState {
    present: bool,
    layout: ColumnLayout,
    saved_view: Option<SheetView>,
    metadata: SheetHistory,
}
#[derive(Debug, Clone, Default)]
struct SheetHistory {
    keys: Vec<String>,
    aggregators: Vec<(String, Aggregator)>,
    derived: Option<DerivedSpec>,
    stale: bool,
    frequency: Option<(String, String)>,
    frequency_baseline: Selection,
}
impl From<&SheetState> for SheetHistory {
    fn from(sheet: &SheetState) -> Self {
        sheet.metadata.clone()
    }
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
    sheets: HashMap<String, SheetHistory>,
    previous_sheet: Option<String>,
    drilldown_return_sheet: Option<String>,
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

/// Data produced by execution, independent of terminal interaction.
#[derive(Debug, Clone)]
pub(super) struct ExecutionOutcome {
    pub(crate) status: PreviousCommand,
    pub(crate) close_history: bool,
    pub(crate) affected_sources: Vec<String>,
    undo: Option<UndoRecord>,
}
impl Default for ExecutionOutcome {
    fn default() -> Self {
        Self {
            status: PreviousCommand::new(Command::None, None),
            close_history: false,
            affected_sources: vec![],
            undo: None,
        }
    }
}

impl Controller {
    pub fn new(database: Database) -> AppResult<Self> {
        let mut sheet_order = database
            .get_table_names()
            .map_err(|error| error.context("Discovering startup sheets"))?;
        sheet_order.retain(|name| name != "table_of_tables" && !name.starts_with(INTERNAL_PREFIX));
        let table_name = sheet_order.first().cloned().unwrap_or_default();
        let revision = database.revision();
        Ok(Self {
            database,
            history: History::default(),
            selected_rows: Selection::default(),
            selected_sheets: HashSet::new(),
            sheets: sheet_order
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        SheetState {
                            present: true,
                            ..SheetState::default()
                        },
                    )
                })
                .collect(),
            sheet_order,
            drilldown_return_sheet: None,
            previous_sheet: None,
            page_key: None,
            source_page: None,
            table_height: 20,
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
            sheet_list_revision: Some(revision),
        })
    }

    fn view_snapshot(&self) -> ViewSnapshot {
        let layout = self
            .sheets
            .get(&self.view.table_name)
            .map(|sheet| sheet.layout.clone())
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
            sheets: self
                .sheets
                .iter()
                .filter(|(_, sheet)| sheet.present)
                .map(|(name, sheet)| (name.clone(), SheetHistory::from(sheet)))
                .collect(),
            previous_sheet: self.previous_sheet.clone(),
            drilldown_return_sheet: self.drilldown_return_sheet.clone(),
        }
    }

    fn restore_view(&mut self, snapshot: ViewSnapshot) {
        let layout = snapshot.layout.clone();
        self.selected_rows = snapshot.selected_rows.clone();
        self.selected_sheets = snapshot.selected_sheets.clone();
        self.sheet_order = snapshot.sheet_order.clone();
        self.previous_sheet = snapshot.previous_sheet.clone();
        self.drilldown_return_sheet = snapshot.drilldown_return_sheet.clone();
        for sheet in self.sheets.values_mut() {
            sheet.metadata = SheetHistory::default();
        }
        for (name, metadata) in snapshot.sheets {
            self.sheets.entry(name).or_default().metadata = metadata;
        }
        self.view.table_name = snapshot.table_name;
        self.view.column = snapshot.column;
        self.view.row = snapshot.row;
        self.view.row_offset = snapshot.row_offset;
        self.view.order_column = snapshot.order_column;
        self.view.ascending = snapshot.ascending;
        self.view.table_before_metadata = snapshot.table_before_metadata;
        self.sheets
            .entry(self.view.table_name.clone())
            .or_default()
            .layout = layout;
    }

    fn commit_history(&mut self, record: UndoRecord, before: ViewSnapshot) {
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
        self.prune_registry();
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
                | Command::RefreshDerived
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
    }

    fn replay_history(&mut self, index: usize) -> AppResult<ExecutionOutcome> {
        if self.drilldown_return_sheet.is_some() {
            return Err(AppError::invalid_input(
                "Frequency source rows are read-only",
            ));
        }
        let entry = self
            .command_history
            .get(index)
            .cloned()
            .ok_or_else(|| AppError::invalid_input("No command selected"))?;
        self.restore_view(entry.snapshot.clone());
        self.selected_rows = entry.selected_rows.clone();
        self.refresh_page(100)?;
        let mut outcome = self.execute_prepared(
            PreparedCommand::new(entry.command, entry.inputs, self)?,
            ExecutionMode::Replay,
        )?;
        outcome.close_history = true;
        outcome.status.message = Some("Replayed command".into());
        Ok(outcome)
    }

    pub(super) fn clear_history(&mut self) -> AppResult<()> {
        let mut result = Ok(());
        for entry in self
            .history
            .undo
            .drain(..)
            .chain(self.history.redo.drain(..))
        {
            result =
                crate::error::finish(result, self.database.discard_hidden_record(&entry.record));
        }
        result
    }

    fn undo(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let Some(mut entry) = self.history.undo.pop_back() else {
            outcome.status = PreviousCommand::new(Command::Undo, Some("Nothing to undo".into()));
            return Ok(outcome);
        };
        if let Err(err) = self.database.toggle_history_record(&mut entry.record) {
            self.history.undo.push_back(entry);
            return Err(err);
        }
        self.apply_registry_history(&entry.record);
        self.restore_view(entry.before.clone());
        self.history.redo.push_back(entry);
        outcome.status = PreviousCommand::new(Command::Undo, Some("Undid last command".into()));
        Ok(outcome)
    }

    fn redo(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let Some(mut entry) = self.history.redo.pop_back() else {
            outcome.status = PreviousCommand::new(Command::Redo, Some("Nothing to redo".into()));
            return Ok(outcome);
        };
        if let Err(err) = self.database.toggle_history_record(&mut entry.record) {
            self.history.redo.push_back(entry);
            return Err(err);
        }
        self.apply_registry_history(&entry.record);
        self.restore_view(entry.after.clone());
        self.history.undo.push_back(entry);
        outcome.status = PreviousCommand::new(Command::Redo, Some("Redid command".into()));
        Ok(outcome)
    }

    pub(crate) fn ordering(&self) -> String {
        if self.view.table_name == "table_of_tables" && !self.sheet_order.is_empty() {
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
            .ok_or_else(|| AppError::invalid_input("Could not get header"))
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
                let count = self.database.count_rows(&self.view.table_name)?;
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
            return Err(AppError::invalid_input(
                "The id column is pinned and cannot be hidden",
            ));
        }
        self.sheets
            .entry(self.view.table_name.clone())
            .or_default()
            .layout
            .hidden
            .insert(name);
        self.view.column = self.view.column.saturating_sub(1);
        Ok(())
    }

    fn show_all_columns(&mut self) {
        self.sheets
            .entry(self.view.table_name.clone())
            .or_default()
            .layout
            .hidden
            .clear();
    }

    fn move_column(&mut self, left: bool) -> AppResult<()> {
        let name = self.selected_column()?;
        if name == "id" {
            return Err(AppError::invalid_input("The id column stays first"));
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
        let layout = &mut self
            .sheets
            .entry(self.view.table_name.clone())
            .or_default()
            .layout;
        let current_index = layout
            .order
            .iter()
            .position(|header| header == &name)
            .ok_or_else(|| AppError::invalid_input("Column layout is out of date"))?;
        let target_index = layout
            .order
            .iter()
            .position(|header| header == &visible[target])
            .ok_or_else(|| AppError::invalid_input("Column layout is out of date"))?;
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
        self.sheets.entry(current.clone()).or_default().saved_view = Some(SheetView {
            column: self.view.column,
            row: self.view.row,
            row_offset: self.view.row_offset,
            order_column: self.view.order_column.clone(),
            ascending: self.view.ascending,
            selection: self.selected_rows.clone(),
        });
        self.previous_sheet = Some(current);
        self.view.table_name = target.clone();
        if let Some(saved) = self
            .sheets
            .get(&target)
            .and_then(|sheet| sheet.saved_view.clone())
        {
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
            return Err(AppError::invalid_input(
                "The highlighted sheet is not registered",
            ));
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

    fn reconcile_registry(&mut self) -> AppResult<()> {
        let names = self.database.get_table_names()?;
        for sheet in self.sheets.values_mut() {
            sheet.present = false;
        }
        for name in &names {
            self.sheets.entry(name.clone()).or_default().present = true;
        }
        self.sheet_order.retain(|name| names.contains(name));
        for name in &names {
            if name != "table_of_tables" && !self.sheet_order.contains(name) {
                self.sheet_order.push(name.clone());
            }
        }
        self.selected_sheets.retain(|name| names.contains(name));
        self.prune_registry();
        for target in [
            &mut self.previous_sheet,
            &mut self.drilldown_return_sheet,
            &mut self.view.table_before_metadata,
        ] {
            if target.as_ref().is_some_and(|name| !names.contains(name)) {
                *target = None;
            }
        }
        Ok(())
    }

    fn prune_registry(&mut self) {
        let retained = self
            .history
            .undo
            .iter()
            .chain(&self.history.redo)
            .flat_map(|entry| entry.before.sheets.keys().chain(entry.after.sheets.keys()))
            .collect::<HashSet<_>>();
        self.sheets
            .retain(|name, sheet| sheet.present || retained.contains(name));
    }

    fn refresh_sheet_list(&mut self) -> AppResult<()> {
        self.database.view_metadata_table()?;
        for (table, sheet) in &self.sheets {
            let Some(spec) = &sheet.metadata.derived else {
                continue;
            };
            let sources = Self::derived_sources(spec);
            self.database.connection.execute(
                "UPDATE table_of_tables SET source_names = ?1, sheet_kind = ?2, stale = ?3 WHERE table_name = ?4",
                rusqlite::params![
                    sources.join(", "),
                    match sheet.metadata.derived.as_ref() {
                        Some(DerivedSpec::Describe { .. }) => "describe",
                        Some(DerivedSpec::Frequency { .. }) => "frequency",
                        Some(DerivedSpec::Group { .. }) => "group",
                        Some(DerivedSpec::Pivot { .. }) => "pivot",
                        Some(DerivedSpec::Append { .. }) => "append",
                        Some(DerivedSpec::Join { .. }) => "join",
                        None => "table",
                    },
                    sheet.metadata.stale as i64,
                    table,
                ],
            )?;
        }
        self.database.invalidate();
        Ok(())
    }

    fn derived_sources(spec: &DerivedSpec) -> Vec<&str> {
        match spec {
            DerivedSpec::Describe { source }
            | DerivedSpec::Frequency { source, .. }
            | DerivedSpec::Group { source, .. }
            | DerivedSpec::Pivot { source, .. } => vec![source],
            DerivedSpec::Append { sources } => sources.iter().map(String::as_str).collect(),
            DerivedSpec::Join { left, right, .. } => vec![left, right],
        }
    }

    fn mark_source_stale(&mut self, source: &str) {
        for sheet in self.sheets.values_mut() {
            if sheet
                .metadata
                .derived
                .as_ref()
                .is_some_and(|spec| Self::derived_sources(spec).contains(&source))
            {
                sheet.metadata.stale = true;
            }
        }
    }
    fn mark_all_derived_stale(&mut self) {
        for sheet in self.sheets.values_mut() {
            if sheet.metadata.derived.is_some() {
                sheet.metadata.stale = true;
            }
        }
    }

    fn command_mutates_source(command: &Command) -> bool {
        matches!(
            command,
            Command::Copy
                | Command::RegexTransform
                | Command::Edit
                | Command::TextToInt
                | Command::IntToText
                | Command::DeleteColumn
                | Command::MathOperation
                | Command::RenameColumn
                | Command::BulkEdit
                | Command::DeleteSelected
                | Command::DeleteTable
        )
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

    fn show_analysis(&mut self, frequency: bool) -> AppResult<ExecutionOutcome> {
        let source = self.view.table_name.clone();
        let spec = if frequency {
            DerivedSpec::Frequency {
                source,
                column: self.selected_column()?,
            }
        } else {
            DerivedSpec::Describe { source }
        };
        self.create_derived_sheet(
            spec,
            if frequency {
                Command::Frequency
            } else {
                Command::Summary
            },
        )
    }

    fn join_table(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let target = input;
        let (right, right_column) = target
            .split_once('.')
            .ok_or_else(|| AppError::invalid_input("Join target must use table.column"))?;
        if right.is_empty() || right_column.is_empty() || right == "table_of_tables" {
            return Err(AppError::invalid_input("Invalid join target"));
        }
        self.create_derived_sheet(
            DerivedSpec::Join {
                left: self.view.table_name.clone(),
                right: right.into(),
                keys: vec![(self.selected_column()?, right_column.into())],
                kind: JoinKind::Inner,
            },
            Command::Join,
        )
    }

    fn toggle_join_key(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        if self.view.table_name == "table_of_tables" {
            return Err(AppError::invalid_input(
                "Mark join keys in each source table",
            ));
        }
        let table = self.view.table_name.clone();
        let column = self.selected_column()?;
        let keys = &mut self.sheets.entry(table).or_default().metadata.keys;
        if let Some(index) = keys.iter().position(|key| key == &column) {
            keys.remove(index);
            outcome.status = PreviousCommand::new(
                Command::ToggleJoinKey,
                Some(format!("Unmarked {column} as a join key")),
            );
        } else {
            keys.push(column.clone());
            outcome.status = PreviousCommand::new(
                Command::ToggleJoinKey,
                Some(format!("Marked {column} as join key #{}", keys.len())),
            );
        }
        Ok(outcome)
    }

    fn combine_selected_tables(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        if self.view.table_name != "table_of_tables" {
            return Err(AppError::invalid_input(
                "Open the sheet list and select tables first",
            ));
        }
        let tables = self
            .sheet_order
            .iter()
            .filter(|name| self.selected_sheets.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        if tables.len() < 2 {
            return Err(AppError::invalid_input(
                "Select at least two tables in the sheet list",
            ));
        }
        let action = input.trim().to_ascii_lowercase();
        let spec = if action == "append" {
            DerivedSpec::Append {
                sources: tables.clone(),
            }
        } else {
            if tables.len() != 2 {
                return Err(AppError::invalid_input(
                    "Joins require exactly two selected tables",
                ));
            }
            let left_keys = self
                .sheets
                .get(&tables[0])
                .map(|sheet| sheet.metadata.keys.clone())
                .unwrap_or_default();
            let right_keys = self
                .sheets
                .get(&tables[1])
                .map(|sheet| sheet.metadata.keys.clone())
                .unwrap_or_default();
            if left_keys.is_empty() || left_keys.len() != right_keys.len() {
                return Err(AppError::invalid_input(
                    "Mark the same number of ordered join keys on both tables",
                ));
            }
            let kind = match action.as_str() {
                "inner" => JoinKind::Inner,
                "left" => JoinKind::Left,
                "full" => JoinKind::Full,
                _ => {
                    return Err(AppError::invalid_input(
                        "Choose append, inner, left, or full",
                    ))
                }
            };
            DerivedSpec::Join {
                left: tables[0].clone(),
                right: tables[1].clone(),
                keys: left_keys.into_iter().zip(right_keys).collect(),
                kind,
            }
        };
        self.create_derived_sheet(spec, Command::CombineTables)
    }

    fn configure_aggregator(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let column = self.selected_column()?;
        let label = input.trim().to_ascii_lowercase();
        let aggregator = match label.as_str() {
            "count" => Aggregator::Count,
            "distinct" | "distinct_count" => Aggregator::DistinctCount,
            "sum" => Aggregator::Sum,
            "avg" | "average" => Aggregator::Average,
            "min" => Aggregator::Min,
            "max" => Aggregator::Max,
            _ => {
                return Err(AppError::invalid_input(
                    "Choose count, distinct, sum, average, min, or max",
                ))
            }
        };
        let table = self.view.table_name.clone();
        let columns = &mut self.sheets.entry(table).or_default().metadata.aggregators;
        if let Some(existing) = columns.iter_mut().find(|(name, _)| name == &column) {
            existing.1 = aggregator;
        } else {
            columns.push((column.clone(), aggregator));
        }
        outcome.status = PreviousCommand::new(
            Command::ConfigureAggregator,
            Some(format!("{} configured for {column}", aggregator.name())),
        );
        Ok(outcome)
    }

    fn create_grouped_sheet(&mut self) -> AppResult<ExecutionOutcome> {
        let source = self.view.table_name.clone();
        let keys = self
            .sheets
            .get(&source)
            .map(|sheet| sheet.metadata.keys.clone())
            .unwrap_or_default();
        let aggregations = self
            .sheets
            .get(&source)
            .map(|sheet| sheet.metadata.aggregators.clone())
            .unwrap_or_default();
        self.create_derived_sheet(
            DerivedSpec::Group {
                source,
                keys,
                aggregations,
            },
            Command::Group,
        )
    }

    fn create_pivot_sheet(&mut self) -> AppResult<ExecutionOutcome> {
        let source = self.view.table_name.clone();
        let pivot_column = self.selected_column()?;
        let keys = self
            .sheets
            .get(&source)
            .map(|sheet| sheet.metadata.keys.clone())
            .unwrap_or_default();
        if keys.contains(&pivot_column) {
            return Err(AppError::invalid_input(
                "The pivot column cannot also be a row key",
            ));
        }
        let aggregations = self
            .sheets
            .get(&source)
            .map(|sheet| sheet.metadata.aggregators.clone())
            .unwrap_or_default();
        self.create_derived_sheet(
            DerivedSpec::Pivot {
                source,
                keys,
                column: pivot_column,
                aggregations,
            },
            Command::Pivot,
        )
    }

    fn create_derived_sheet(
        &mut self,
        spec: DerivedSpec,
        command: Command,
    ) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let (result, membership) = self.build_derived_sheet(&spec).map_err(|error| {
            error.context(format!(
                "Constructing sheet from '{}'",
                self.view.table_name
            ))
        })?;
        let sources = Self::derived_sources(&spec)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Some(membership) = membership {
            let sheet = self.sheets.entry(result.clone()).or_default();
            sheet.metadata.frequency = Some((sources[0].clone(), membership));
            sheet.metadata.frequency_baseline = self.selected_rows.clone();
        }
        let source = sources.join(", ");
        let sheet = self.sheets.entry(result.clone()).or_default();
        sheet.present = true;
        sheet.metadata.derived = Some(spec);
        sheet.metadata.stale = false;
        if command == Command::CombineTables {
            self.selected_sheets.clear();
        }
        let hidden_name = self.database.internal_table_name()?;
        self.switch_sheet(result.clone())?;
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        self.view.order_column = Some("id".into());
        self.view.ascending = true;
        outcome.undo = Some(UndoRecord::Visibility {
            table: None,
            visible_name: result.clone(),
            hidden_name,
            is_visible: true,
        });
        outcome.status =
            PreviousCommand::new(command, Some(format!("Created result from {source}")));
        Ok(outcome)
    }

    fn build_derived_sheet(&self, spec: &DerivedSpec) -> AppResult<(String, Option<String>)> {
        Ok(match spec {
            DerivedSpec::Describe { source } => (self.database.describe_table(source)?, None),
            DerivedSpec::Frequency { source, column } => {
                let (table, membership) =
                    self.database.frequency_table_with_members(source, column)?;
                (table, Some(membership))
            }
            DerivedSpec::Group {
                source,
                keys,
                aggregations,
            } => (
                self.database.grouped_table(source, keys, aggregations)?,
                None,
            ),
            DerivedSpec::Pivot {
                source,
                keys,
                column,
                aggregations,
            } => (
                self.database
                    .pivot_table(source, keys, column, aggregations)?,
                None,
            ),
            DerivedSpec::Append { sources } => (self.database.append_tables(sources)?, None),
            DerivedSpec::Join {
                left,
                right,
                keys,
                kind,
            } => (
                self.database
                    .join_tables_with_keys(left, right, keys, *kind)?,
                None,
            ),
        })
    }

    fn refresh_derived_sheet(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let name = self.view.table_name.clone();
        let Some(spec) = self
            .sheets
            .get(&name)
            .and_then(|sheet| sheet.metadata.derived.clone())
        else {
            return Err(AppError::invalid_input(
                "The current sheet has no refreshable source",
            ));
        };
        if !self
            .sheets
            .get(&name)
            .is_some_and(|sheet| sheet.metadata.stale)
        {
            outcome.status = PreviousCommand::new(
                Command::RefreshDerived,
                Some("Derived sheet is already up to date".into()),
            );
            return Ok(outcome);
        }
        let old_membership = self
            .sheets
            .get(&name)
            .and_then(|sheet| sheet.metadata.frequency.as_ref())
            .map(|(_, membership)| membership.clone());
        let new_membership = self.database.atomic(|| {
            let (generated, membership) = self.build_derived_sheet(&spec)?;
            if let Some(membership) = &old_membership {
                self.database.connection.execute_batch(&format!(
                    "DROP TABLE IF EXISTS \"{}\";",
                    membership.replace('"', "\"\"")
                ))?;
            }
            self.database.connection.execute_batch(&format!(
                "DROP TABLE IF EXISTS \"{}\";",
                name.replace('"', "\"\"")
            ))?;
            self.database.invalidate();
            self.database.rename_table(&generated, &name)?;
            Ok(membership)
        })?;
        if let Some(membership) = new_membership {
            if let Some((source, _)) = self
                .sheets
                .get(&name)
                .and_then(|sheet| sheet.metadata.frequency.clone())
            {
                self.sheets
                    .entry(name.clone())
                    .or_default()
                    .metadata
                    .frequency = Some((source, membership));
            }
        }
        self.sheets.entry(name.clone()).or_default().metadata.stale = false;
        outcome.status = PreviousCommand::new(
            Command::RefreshDerived,
            Some("Rebuilt derived sheet from its sources".into()),
        );
        Ok(outcome)
    }

    fn toggle_selection(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        if self.view.table_name == "table_of_tables" {
            let name = self.highlighted_table_name()?;
            if !self.selected_sheets.insert(name.clone()) {
                self.selected_sheets.remove(&name);
            }
            outcome.status = PreviousCommand::new(
                Command::ToggleSelection,
                Some(format!("{} tables selected", self.selected_sheets.len())),
            );
            return Ok(outcome);
        }
        let id = self
            .view
            .rows
            .get(self.view.row)
            .and_then(|row| row.get(*self.view.column_indexes.first()?))
            .ok_or_else(|| AppError::invalid_input("No selected row"))?
            .to_string()
            .parse::<i64>()
            .map_err(|err| AppError::invalid_input(err.to_string()))?;
        if !self.selected_rows.insert(id) {
            self.selected_rows.remove(&id);
        }
        self.sync_frequency_source_selection()?;
        outcome.status = PreviousCommand::new(
            Command::ToggleSelection,
            Some(format!("{} rows selected", self.selected_rows.len())),
        );
        Ok(outcome)
    }

    fn clear_selection(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        if self.view.table_name == "table_of_tables" {
            self.selected_sheets.clear();
            outcome.status = PreviousCommand::new(
                Command::ClearSelection,
                Some("Table selection cleared".into()),
            );
            return Ok(outcome);
        }
        self.selected_rows.clear();
        self.sync_frequency_source_selection()?;
        outcome.status =
            PreviousCommand::new(Command::ClearSelection, Some("Selection cleared".into()));
        Ok(outcome)
    }

    fn sync_frequency_source_selection(&mut self) -> AppResult<()> {
        let Some((source, membership)) = self
            .sheets
            .get(&self.view.table_name)
            .and_then(|sheet| sheet.metadata.frequency.clone())
        else {
            return Ok(());
        };
        let mut selected = self
            .sheets
            .get(&self.view.table_name)
            .map(|sheet| sheet.metadata.frequency_baseline.clone())
            .unwrap_or_default();
        for group_id in &*self.selected_rows {
            selected.extend(
                self.database
                    .frequency_group_members(&membership, *group_id)?,
            );
        }
        if let Some(source_view) = self
            .sheets
            .get_mut(&source)
            .and_then(|sheet| sheet.saved_view.as_mut())
        {
            source_view.selection = selected;
        }
        Ok(())
    }

    fn drill_into_frequency_group(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let current = self.view.table_name.clone();
        let Some((source, membership)) = self
            .sheets
            .get(&current)
            .and_then(|sheet| sheet.metadata.frequency.clone())
        else {
            return Ok(outcome);
        };
        let row = self
            .view
            .rows
            .get(self.view.row)
            .ok_or_else(|| AppError::invalid_input("No frequency group is highlighted"))?;
        let group_id = row
            .get(
                *self
                    .view
                    .column_indexes
                    .first()
                    .ok_or_else(|| AppError::invalid_input("Frequency ID column is hidden"))?,
            )
            .ok_or_else(|| AppError::invalid_input("Frequency ID is unavailable"))?
            .to_string()
            .parse::<i64>()
            .map_err(|error| AppError::invalid_input(error.to_string()))?;
        let members = self
            .database
            .frequency_group_members(&membership, group_id)?;
        let mut selection = self
            .sheets
            .get(&current)
            .map(|sheet| sheet.metadata.frequency_baseline.clone())
            .unwrap_or_default();
        selection.extend(members);
        if let Some(source_view) = self
            .sheets
            .get_mut(&source)
            .and_then(|sheet| sheet.saved_view.as_mut())
        {
            source_view.selection = selection;
        }
        self.switch_sheet(source)?;
        self.drilldown_return_sheet = Some(current);
        outcome.status = PreviousCommand::new(
            Command::HistoryReplay,
            Some("Source rows highlighted; Esc returns to frequencies".into()),
        );
        Ok(outcome)
    }

    fn leave_frequency_drilldown(&mut self) -> AppResult<()> {
        let Some(target) = self.drilldown_return_sheet.take() else {
            return self.previous_sheet();
        };
        self.switch_sheet(target)?;
        Ok(())
    }
    fn select_sql(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let condition = input;
        let table = self.view.table_name.replace('"', "\"\"");
        let mut stmt = self
            .database
            .connection
            .prepare(&format!("SELECT id FROM \"{table}\" WHERE {condition}"))?;
        let ids = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        self.selected_rows.extend(ids);
        outcome.status = PreviousCommand::new(
            Command::SelectSql,
            Some(format!("{} rows selected", self.selected_rows.len())),
        );
        Ok(outcome)
    }
    fn select_regex(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let pattern = input;
        let column = self.selected_column()?;
        let ids = self
            .database
            .select_regex(&self.view.table_name, &column, pattern)?;
        self.selected_rows.extend(ids);
        outcome.status = PreviousCommand::new(
            Command::SelectRegex,
            Some(format!("{} rows selected", self.selected_rows.len())),
        );
        Ok(outcome)
    }
    fn delete_selected(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        if self.selected_rows.is_empty() {
            return Err(AppError::invalid_input("No rows selected"));
        }
        let table = self.view.table_name.clone();
        let record = self.database.delete_selected(&table, &self.selected_rows)?;
        outcome.undo = Some(record);
        self.selected_rows.clear();
        outcome.status = PreviousCommand::new(
            Command::DeleteSelected,
            Some("Deleted selected rows".into()),
        );
        Ok(outcome)
    }
    fn copy_selected(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        if self.selected_rows.is_empty() {
            return Err(AppError::invalid_input("No rows selected"));
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
        self.view.table_name = target.clone();
        self.selected_rows.clear();
        self.view.row = 0;
        self.view.row_offset = 0;
        self.view.column = 0;
        outcome.undo = Some(UndoRecord::Visibility {
            table: None,
            visible_name: target,
            hidden_name: hidden,
            is_visible: true,
        });
        Ok(outcome)
    }

    fn bulk_edit(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        if self.selected_rows.is_empty() {
            return Err(AppError::invalid_input(
                "Select rows with Space before bulk editing",
            ));
        }
        let table = self.view.table_name.clone();
        let column = self.selected_column()?;
        if column == "id" {
            return Err(AppError::invalid_input("The id column is read-only"));
        }
        let value = input;
        let record =
            self.database
                .bulk_edit_selected(&table, &column, &self.selected_rows, value)?;
        outcome.undo = Some(record);
        outcome.status =
            PreviousCommand::new(Command::BulkEdit, Some("Updated selected rows".into()));
        Ok(outcome)
    }

    fn delete_table(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
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
                return Err(AppError::invalid_input(
                    "The metadata table cannot be deleted",
                ));
            }
            let hidden = self.database.delete_tables(&tables)?;
            outcome.affected_sources = tables.clone();
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
            outcome.undo = Some(UndoRecord::Group(records));
            outcome.status = PreviousCommand::new(
                Command::DeleteTable,
                Some(format!(
                    "Deleted {} table(s); Ctrl+Z restores them",
                    tables.len()
                )),
            );
            return Ok(outcome);
        }
        let table = self.view.table_name.clone();
        if table == "table_of_tables" {
            return Err(AppError::invalid_input(
                "The metadata table cannot be deleted",
            ));
        }
        let hidden_name = self.database.delete_table(&table)?;
        self.sheet_order.retain(|name| name != &table);
        if let Some(target) = self.sheet_order.last().cloned() {
            self.switch_sheet(target)?;
        } else {
            self.view.table_name = "table_of_tables".into();
            self.view.column = 0;
            self.view.row = 0;
            self.view.row_offset = 0;
        }
        outcome.undo = Some(UndoRecord::Visibility {
            table: None,
            visible_name: table.clone(),
            hidden_name,
            is_visible: false,
        });
        outcome.status = PreviousCommand::new(
            Command::DeleteTable,
            Some(format!("Deleted {table}; Ctrl+Z restores it")),
        );
        Ok(outcome)
    }

    fn highlighted_table_name(&self) -> AppResult<String> {
        if self.view.table_name != "table_of_tables" {
            return Err(AppError::invalid_input("The sheet list is not open"));
        }
        let row = self.view.row;
        let display_column = self
            .view
            .headers
            .iter()
            .position(|header| header == "table_name")
            .ok_or_else(|| AppError::invalid_input("Sheet list has no table_name column"))?;
        let raw_column = *self
            .view
            .column_indexes
            .get(display_column)
            .ok_or_else(|| AppError::invalid_input("Sheet list layout is out of date"))?;
        match self
            .view
            .rows
            .get(row)
            .and_then(|values| values.get(raw_column))
        {
            Some(DataItem::Text(name)) => Ok(name.clone()),
            Some(value) => Ok(value.to_string()),
            None => Err(AppError::invalid_input("No table is highlighted")),
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
        self.reconcile_sheet_order()?;
        self.sheet_list_revision = None;
        self.view.table_before_metadata = Some(self.view.table_name.clone());
        self.view.table_name = "table_of_tables".into();
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        Ok(())
    }

    fn sql_query(&self, input: &str) -> AppResult<()> {
        self.database.sql_query(input)
    }

    fn execute_prepared(
        &mut self,
        prepared: PreparedCommand,
        mode: ExecutionMode,
    ) -> AppResult<ExecutionOutcome> {
        let PreparedCommand {
            action,
            command,
            inputs,
        } = prepared;

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
            return Err(AppError::invalid_input(
                "Frequency source rows are read-only",
            ));
        }
        let mutation_source = self.view.table_name.clone();
        let revision_before = self.database.revision();
        let before_command = self.view_snapshot();
        let logged_inputs = inputs.clone();
        let input = inputs.first().map(String::as_str).unwrap_or_default();
        let height = self.table_height;
        let result = match action {
            PreparedAction::OpenSheet => self
                .open_highlighted_table()
                .map(|()| ExecutionOutcome::default()),
            PreparedAction::DrillFrequency => self.drill_into_frequency_group(),
            PreparedAction::LeaveFrequency => self
                .leave_frequency_drilldown()
                .map(|()| ExecutionOutcome::default()),
            PreparedAction::PreviousSheet => {
                self.previous_sheet().map(|()| ExecutionOutcome::default())
            }
            PreparedAction::Run => match command {
                Command::Quit => Ok(ExecutionOutcome::default()),
                Command::Copy => self.copy_operation(),
                Command::IllegalOperation => Ok(ExecutionOutcome::default()),
                Command::None => Ok(ExecutionOutcome::default()),
                Command::Sort => self.sort(),
                Command::Move(direction) => self
                    .move_cursor(direction, height)
                    .map(|()| ExecutionOutcome::default()),
                Command::NextTable => self
                    .change_table(true)
                    .map(|()| ExecutionOutcome::default()),
                Command::PrevTable => self
                    .change_table(false)
                    .map(|()| ExecutionOutcome::default()),
                Command::PreviousSheet => {
                    self.previous_sheet().map(|()| ExecutionOutcome::default())
                }
                Command::TextToInt => self.text_to_int(),
                Command::IntToText => self.int_to_text(),
                Command::DeleteColumn => self.delete_column(),
                Command::ToggleJoinKey => self.toggle_join_key(),
                Command::Group => self.create_grouped_sheet(),
                Command::Pivot => self.create_pivot_sheet(),
                Command::RefreshDerived => self.refresh_derived_sheet(),
                Command::DeleteTable => self.delete_table(),
                Command::ToggleMetadataTable => self
                    .toggle_metadata_table()
                    .map(|()| ExecutionOutcome::default()),
                Command::Undo => self.undo(),
                Command::Redo => self.redo(),
                Command::Summary => self.show_analysis(false),
                Command::Frequency => self.show_analysis(true),
                Command::ToggleSelection => self.toggle_selection(),
                Command::ClearSelection => self.clear_selection(),
                Command::DeleteSelected if self.view.table_name == "table_of_tables" => {
                    self.delete_table()
                }
                Command::DeleteSelected => self.delete_selected(),
                Command::CopySelected => self.copy_selected(),
                Command::HideColumn => self
                    .hide_selected_column()
                    .map(|()| ExecutionOutcome::default()),
                Command::ShowColumns => {
                    self.show_all_columns();
                    Ok(ExecutionOutcome::default())
                }
                Command::ColumnLeft if self.view.table_name == "table_of_tables" => self
                    .move_sheet_order(true)
                    .map(|()| ExecutionOutcome::default()),
                Command::ColumnRight if self.view.table_name == "table_of_tables" => self
                    .move_sheet_order(false)
                    .map(|()| ExecutionOutcome::default()),
                Command::ColumnLeft => self.move_column(true).map(|()| ExecutionOutcome::default()),
                Command::ColumnRight => self
                    .move_column(false)
                    .map(|()| ExecutionOutcome::default()),
                Command::History => Ok(ExecutionOutcome::default()),
                Command::HistoryReplay | Command::HistoryClose => Ok(ExecutionOutcome::default()),
                Command::RegexTransform => {
                    self.regex_transform_operation(input, inputs.get(1).map(String::as_str))
                }
                Command::Edit => self.edit_cell(input),
                Command::SqlQuery => self.sql_query(input).map(|()| ExecutionOutcome::default()),
                Command::RegexFilter => self.regex_filter_operation(input),
                Command::RenameColumn => self.rename_column(input),
                Command::RenameTable => self.rename_table(input),
                Command::ExactSearch => self.exact_search(input),
                Command::MathOperation => self.math_operation(input),
                Command::Join => self.join_table(input),
                Command::CombineTables => self.combine_selected_tables(input),
                Command::ConfigureAggregator => self.configure_aggregator(input),
                Command::BulkEdit => self.bulk_edit(input),
                Command::SelectSql => self.select_sql(input),
                Command::SelectRegex => self.select_regex(input),
                Command::Save => self.save(input).map(|()| ExecutionOutcome::default()),
            },
        };
        let mut outcome = result?;
        outcome.status.command = command.clone();
        if self.database.revision() != revision_before {
            if outcome.affected_sources.is_empty() {
                outcome.affected_sources.push(mutation_source);
            }
            if command == Command::SqlQuery {
                self.mark_all_derived_stale();
            } else if Self::command_mutates_source(&command) {
                for source in &outcome.affected_sources {
                    self.mark_source_stale(source);
                }
            }
        }
        if let Some(record) = outcome.undo.take() {
            self.sheets
                .entry(self.view.table_name.clone())
                .or_default()
                .present = true;
            self.commit_history(record, before_command.clone());
        }
        if command == Command::SqlQuery {
            self.clear_history()
                .map_err(|error| error.context("SQL completed; undo history cleanup failed"))?;
            outcome.status.message =
                Some("SQL executed; undo history was cleared (SQL is not undoable)".into());
        } else if mode == ExecutionMode::Normal {
            self.log_command(command.clone(), logged_inputs, before_command);
        }
        if outcome.status.message.is_none() && command.needs_input() {
            outcome.status.message = Some("Completed".into());
        }
        Ok(outcome)
    }

    fn save(&self, input: &str) -> AppResult<()> {
        let path = PathBuf::from(input);
        match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
            "csv" => savers::csv(&self.database, &self.view.table_name, &path),
            "tsv" => savers::tsv(&self.database, &self.view.table_name, &path),
            "sqlite" | "db" | "sql" | "sqlite3" => savers::sqlite(&self.database, &path),
            _ => Err(AppError::from("Unsupported file format")),
        }
    }

    /// Compatibility facade for callers that do not use the database worker.
    pub fn execute(&mut self, command: Command, inputs: Vec<String>) -> AppResult<()> {
        // Resolve the initial cursor against the database once. Later executions
        // must still run if presentation of a previous committed command failed.
        if self.source_page.is_none() {
            self.refresh_page(100)?;
        }
        let (execution, snapshot) = self.execute_and_snapshot(command, inputs, None);
        crate::error::finish(execution.map(|_| ()), snapshot.map(|_| ()))
    }

    pub(super) fn special_selection(&self) -> bool {
        self.view.table_name == "table_of_tables"
            || self
                .sheets
                .get(&self.view.table_name)
                .is_some_and(|sheet| sheet.metadata.frequency.is_some())
    }
    pub(super) fn prepare_snapshot(&mut self) -> AppResult<super::worker::DatabaseSnapshot> {
        if self.sheet_list_revision != Some(self.database.revision()) {
            self.refresh_sheet_list()?;
            self.reconcile_registry()?;
            self.sheet_list_revision = Some(self.database.revision());
        }
        self.refresh_page(100)?;
        super::worker::DatabaseSnapshot::from_controller(self)
    }

    pub(super) fn execute_and_snapshot(
        &mut self,
        command: Command,
        inputs: Vec<String>,
        replay_index: Option<usize>,
    ) -> (
        AppResult<ExecutionOutcome>,
        AppResult<super::worker::DatabaseSnapshot>,
    ) {
        let execution = self
            .database
            .check_cancelled()
            .and_then(|()| {
                if let Some(index) = replay_index.filter(|_| command == Command::HistoryReplay) {
                    self.replay_history(index)
                } else {
                    PreparedCommand::new(command, inputs, self)
                        .and_then(|prepared| self.execute_prepared(prepared, ExecutionMode::Normal))
                }
            })
            .map_err(|error| {
                error.normalize_cancelled(
                    self.database
                        .cancellation
                        .load(std::sync::atomic::Ordering::Relaxed),
                )
            });
        self.database
            .cancellation
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let snapshot = self.prepare_snapshot().map_err(|error| {
            error.context(if execution.is_ok() {
                "Command completed; page could not be loaded"
            } else {
                "Loading page after failed command"
            })
        });
        (execution, snapshot)
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
            move |token| {
                Database::load_cancellable(paths, token)
                    .and_then(Self::new)
                    .map_err(|error| error.context("Starting Dataman"))
            },
            tui,
        )
    }

    pub(super) fn shutdown(&mut self) -> AppResult<()> {
        self.clear_history()
    }

    pub fn get_headers_and_rows(&mut self, limit: u32) -> AppResult<DataTable> {
        self.refresh_page(limit)?;
        self.database.count_rows(&self.view.table_name)?;
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
        self.normalize_sort_column()?;
        let ordering = self.ordering();
        let layout = self
            .sheets
            .get(&self.view.table_name)
            .map(|sheet| sheet.layout.clone())
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
        let page = self
            .source_page
            .as_ref()
            .ok_or_else(|| AppError::from("Page cache is unavailable"))?;
        let layout = &mut self
            .sheets
            .entry(self.view.table_name.clone())
            .or_default()
            .layout;
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
        self.execute(Command::RegexFilter, inputs)
    }

    fn regex_filter_operation(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let pattern = input.to_owned();
        let header = self.selected_column()?;
        let filtered_table =
            self.database
                .regex_filter(&self.view.table_name, &header, &pattern)?;
        let hidden_name = self.database.internal_table_name()?;
        self.view.table_name = filtered_table.clone();
        self.view.column = 0;
        self.view.row = 0;
        self.view.row_offset = 0;
        outcome.undo = Some(UndoRecord::Visibility {
            table: None,
            visible_name: filtered_table,
            hidden_name,
            is_visible: true,
        });
        Ok(outcome)
    }

    /// The user enters a regex and we will derive a new column from that.
    /// If the user enter a capture group we will ask for a second input that shows how the capture group should be transformed.
    /// If the user doesn't enter a capture group we will just copy the regex match.
    pub fn regex_transform(&mut self, inputs: Vec<String>) -> AppResult<()> {
        self.execute(Command::RegexTransform, inputs)
    }

    fn regex_transform_operation(
        &mut self,
        input: &str,
        transformation: Option<&str>,
    ) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        let pattern = input.to_owned();
        let regex = regex::Regex::new(&pattern)?;
        let contains_capture_pattern = regex.capture_names().len() > 1;
        let header = self.selected_column()?;

        if let Some(transformation) = transformation {
            if contains_capture_pattern && !transformation.is_empty() {
                self.database.regex_capture_group_transform(
                    &self.view.table_name,
                    &pattern,
                    &header,
                    transformation,
                )?;
                outcome.undo = self.record_added_column(&table, &old_headers)?.undo;
                return Ok(outcome);
            }
        }
        self.database
            .regex_no_capture_group_transform(&self.view.table_name, &pattern, &header)?;
        outcome.undo = self.record_added_column(&table, &old_headers)?.undo;
        Ok(outcome)
    }

    fn record_added_column(
        &mut self,
        table: &str,
        old_headers: &[String],
    ) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let new_column = self
            .database
            .get_headers(table)?
            .into_iter()
            .find(|header| !old_headers.contains(header))
            .ok_or_else(|| AppError::invalid_input("Operation did not create a column"))?;
        let hidden_name = self.database.internal_column_name(table)?;
        outcome.undo = Some(UndoRecord::Visibility {
            table: Some(table.to_owned()),
            visible_name: new_column,
            hidden_name,
            is_visible: true,
        });
        Ok(outcome)
    }

    pub fn copy(&mut self) -> AppResult<()> {
        self.execute(Command::Copy, vec![])
    }

    fn copy_operation(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        self.database
            .copy(&self.view.table_name, &self.selected_column()?)?;
        outcome.undo = self.record_added_column(&table, &old_headers)?.undo;
        outcome.status = PreviousCommand::new(Command::Copy, None);
        Ok(outcome)
    }

    fn edit_cell(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let header = self.selected_column()?;
        if header == "id" {
            return Err(AppError::invalid_input("The id column is read-only"));
        }
        let table = self.view.table_name.clone();
        let id = self
            .view
            .rows
            .get(self.view.row)
            .and_then(|r| r.get(*self.view.column_indexes.first()?))
            .map(|v| v.to_string())
            .ok_or_else(|| AppError::invalid_input("No selected row"))?
            .parse::<i64>()
            .map_err(|e| AppError::invalid_input(e.to_string()))?;
        let data = self.database.get_cell_item(&table, id, &header)?;
        let edit_result = input.to_owned();

        self.database
            .update_cell(&self.view.table_name, header.as_str(), id, &edit_result)?;
        outcome.undo = Some(UndoRecord::CellEdit {
            table,
            column: header,
            id,
            before: data,
            after: DataItem::Text(edit_result),
            is_after: true,
        });
        Ok(outcome)
    }

    fn sort(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let header = self.selected_column()?;
        if self.view.order_column.as_deref() == Some(&header) {
            self.view.ascending = !self.view.ascending;
        } else {
            self.view.order_column = Some(header);
            self.view.ascending = true;
        }
        self.view.row = 0;
        self.view.row_offset = 0;
        outcome.undo = Some(UndoRecord::ViewChange);
        Ok(outcome)
    }
    fn exact_search(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let pattern = input.to_owned();
        let header = self.selected_column()?;
        let current = self.view.row_offset + self.view.row as u32;
        match self.database.exact_search(
            &self.view.table_name,
            &self.ordering(),
            current,
            &header,
            &pattern,
        )? {
            Some(position) => {
                let height = self.table_height.max(1);
                self.view.row = (position % height) as usize;
                self.view.row_offset = position - position % height;
                outcome.status =
                    PreviousCommand::new(Command::ExactSearch, Some("Match found".to_string()));
            }
            None => {
                outcome.status =
                    PreviousCommand::new(Command::ExactSearch, Some("No match found".to_string()));
            }
        }
        Ok(outcome)
    }

    fn text_to_int(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        outcome.status = PreviousCommand::new(Command::TextToInt, None);
        self.database
            .text_to_int(&table, &self.selected_column()?)?;
        self.record_added_column(&table, &old_headers)
    }

    fn int_to_text(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        outcome.status = PreviousCommand::new(Command::IntToText, None);
        self.database
            .int_to_text(&table, &self.selected_column()?)?;
        self.record_added_column(&table, &old_headers)
    }

    fn delete_column(&mut self) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let table = self.view.table_name.clone();
        let column = self.selected_column()?;
        let hidden_name = self
            .database
            .delete_column(&self.view.table_name, &column)?;
        if let Some(keys) = self
            .sheets
            .get_mut(&table)
            .map(|sheet| &mut sheet.metadata.keys)
        {
            keys.retain(|key| key != &column);
        }
        if self.view.order_column.as_deref() == Some(&column) {
            self.view.order_column = None;
        }
        outcome.status = PreviousCommand::new(
            Command::DeleteColumn,
            Some(format!("Deleted column {column}")),
        );
        outcome.undo = Some(UndoRecord::Visibility {
            table: Some(table),
            visible_name: column,
            hidden_name,
            is_visible: false,
        });
        Ok(outcome)
    }

    fn rename_column(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let table = self.view.table_name.clone();
        let new_column = input.to_owned();
        if new_column.starts_with(INTERNAL_PREFIX) {
            return Err(AppError::invalid_input(
                "Column names using Dataman's reserved undo prefix are not supported",
            ));
        }
        let old_column = self.selected_column()?;
        self.database
            .rename_column(&self.view.table_name, &old_column, &new_column)?;
        self.rename_column_references(&table, &old_column, &new_column);
        if self.view.order_column.as_deref() == Some(&old_column) {
            self.view.order_column = Some(new_column.clone());
        }
        outcome.status = PreviousCommand::new(Command::RenameColumn, None);
        outcome.undo = Some(UndoRecord::RenameColumn {
            table,
            before: old_column,
            after: new_column,
            is_after: true,
        });
        Ok(outcome)
    }

    fn math_operation(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let table = self.view.table_name.clone();
        let old_headers = self.database.get_headers(&table)?;
        self.database
            .math_operation(&table, vec![input.to_owned()])?;
        self.record_added_column(&table, &old_headers)
    }

    pub(super) fn marked_join_keys(&self) -> Vec<String> {
        self.sheets
            .get(&self.view.table_name)
            .map(|sheet| sheet.metadata.keys.clone())
            .unwrap_or_default()
    }

    fn apply_registry_history(&mut self, record: &UndoRecord) {
        match record {
            UndoRecord::RenameTable {
                before,
                after,
                is_after,
            } => {
                if *is_after {
                    self.rename_sheet_references(before, after);
                } else {
                    self.rename_sheet_references(after, before);
                }
            }
            UndoRecord::RenameColumn {
                table,
                before,
                after,
                is_after,
            } => {
                if *is_after {
                    self.rename_column_references(table, before, after);
                } else {
                    self.rename_column_references(table, after, before);
                }
            }
            UndoRecord::Group(records) => {
                for record in records {
                    self.apply_registry_history(record);
                }
            }
            _ => {}
        }
    }

    fn rename_column_references(&mut self, table: &str, old: &str, new: &str) {
        fn rewrite(value: &mut String, old: &str, new: &str) {
            if value == old {
                *value = new.to_owned();
            }
        }
        for (name, sheet) in &mut self.sheets {
            if name == table {
                for column in &mut sheet.layout.order {
                    rewrite(column, old, new);
                }
                if sheet.layout.hidden.remove(old) {
                    sheet.layout.hidden.insert(new.to_owned());
                }
                for key in &mut sheet.metadata.keys {
                    rewrite(key, old, new);
                }
                for (column, _) in &mut sheet.metadata.aggregators {
                    rewrite(column, old, new);
                }
                if let Some(saved) = &mut sheet.saved_view {
                    if let Some(column) = &mut saved.order_column {
                        rewrite(column, old, new);
                    }
                }
            }
            match &mut sheet.metadata.derived {
                Some(DerivedSpec::Frequency { source, column }) if source == table => {
                    rewrite(column, old, new)
                }
                Some(DerivedSpec::Group {
                    source,
                    keys,
                    aggregations,
                })
                | Some(DerivedSpec::Pivot {
                    source,
                    keys,
                    aggregations,
                    ..
                }) if source == table => {
                    for key in keys {
                        rewrite(key, old, new);
                    }
                    for (column, _) in aggregations {
                        rewrite(column, old, new);
                    }
                    if let Some(DerivedSpec::Pivot { column, .. }) = &mut sheet.metadata.derived {
                        rewrite(column, old, new);
                    }
                }
                Some(DerivedSpec::Join {
                    left, right, keys, ..
                }) => {
                    for (left_key, right_key) in keys {
                        if left == table {
                            rewrite(left_key, old, new);
                        }
                        if right == table {
                            rewrite(right_key, old, new);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn rename_sheet_references(&mut self, old: &str, new: &str) {
        if let Some(sheet) = self.sheets.remove(old) {
            self.sheets.insert(new.to_owned(), sheet);
        }
        for name in &mut self.sheet_order {
            if name == old {
                *name = new.to_owned();
            }
        }
        if self.selected_sheets.remove(old) {
            self.selected_sheets.insert(new.to_owned());
        }
        for target in [
            &mut self.previous_sheet,
            &mut self.drilldown_return_sheet,
            &mut self.view.table_before_metadata,
        ] {
            if target.as_deref() == Some(old) {
                *target = Some(new.to_owned());
            }
        }
        for sheet in self.sheets.values_mut() {
            if let Some((source, _)) = &mut sheet.metadata.frequency {
                if source == old {
                    *source = new.to_owned();
                }
            }
            if let Some(spec) = &mut sheet.metadata.derived {
                let sources = match spec {
                    DerivedSpec::Describe { source }
                    | DerivedSpec::Frequency { source, .. }
                    | DerivedSpec::Group { source, .. }
                    | DerivedSpec::Pivot { source, .. } => vec![source],
                    DerivedSpec::Append { sources } => sources.iter_mut().collect(),
                    DerivedSpec::Join { left, right, .. } => vec![left, right],
                };
                for source in sources {
                    if source == old {
                        *source = new.to_owned();
                    }
                }
            }
        }
    }

    fn rename_table(&mut self, input: &str) -> AppResult<ExecutionOutcome> {
        let mut outcome = ExecutionOutcome::default();
        let new_table_name = input.to_owned();
        let from_sheet_list = self.view.table_name == "table_of_tables";
        let old_table_name = if from_sheet_list {
            self.highlighted_table_name()?
        } else {
            self.view.table_name.clone()
        };
        if old_table_name == "table_of_tables" {
            return Err(AppError::invalid_input(
                "The metadata table cannot be renamed",
            ));
        }
        if new_table_name.starts_with(INTERNAL_PREFIX) {
            return Err(AppError::invalid_input(
                "Table names using Dataman's reserved undo prefix are not supported",
            ));
        }
        self.database
            .rename_table(&old_table_name, &new_table_name)?;
        let renamed_table_name = new_table_name.clone();
        self.rename_sheet_references(&old_table_name, &new_table_name);
        if !from_sheet_list {
            self.view.table_name = new_table_name;
        }
        outcome.status = PreviousCommand::new(Command::RenameTable, None);
        outcome.undo = Some(UndoRecord::RenameTable {
            before: old_table_name,
            after: if from_sheet_list {
                renamed_table_name
            } else {
                self.view.table_name.clone()
            },
            is_after: true,
        });
        Ok(outcome)
    }
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
        let mut controller = Controller::new(database).unwrap();
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
    fn join_shortcut_and_refresh_share_schema_and_preserve_source_view() {
        let mut controller = controller();
        controller.database.execute_batch("CREATE TABLE scores (id INTEGER PRIMARY KEY, name TEXT, score INTEGER); INSERT INTO scores(name, score) VALUES ('abc', 9);").unwrap();
        controller.view.row = 1;
        controller.selected_rows.insert(2);
        controller
            .execute(Command::Join, vec!["scores.name".into()])
            .unwrap();
        let result = controller.view.table_name.clone();
        let headers = controller.database.get_headers(&result).unwrap();
        assert!(headers.contains(&"t_name".into()));
        assert!(headers.contains(&"scores_score".into()));
        controller.switch_sheet("t".into()).unwrap();
        assert_eq!(controller.view.row, 1);
        assert!(controller.selected_rows.contains(&2));
        controller
            .execute(
                Command::SqlQuery,
                vec!["UPDATE scores SET score = 10".into()],
            )
            .unwrap();
        controller.switch_sheet(result.clone()).unwrap();
        controller.execute(Command::RefreshDerived, vec![]).unwrap();
        assert_eq!(controller.database.get_headers(&result).unwrap(), headers);
        assert_eq!(
            controller
                .database
                .get_cell(&result, 1, "scores_score")
                .unwrap(),
            "10"
        );
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
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
        controller.refresh_page(20).unwrap();
        assert_eq!(controller.highlighted_table_name().unwrap(), "t");
        controller.open_highlighted_table().unwrap();
        assert_eq!(controller.view.table_name, "t");

        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
        controller.refresh_page(20).unwrap();
        controller.execute(Command::DeleteTable, vec![]).unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        assert_eq!(controller.view.table_name, "table_of_tables");

        controller.execute(Command::Undo, vec![]).unwrap();
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
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
        controller.refresh_page(20).unwrap();
        controller
            .execute(Command::RenameTable, vec!["renamed".into()])
            .unwrap();
        assert_eq!(controller.view.table_name, "table_of_tables");
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"renamed".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
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
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
        controller.refresh_page(20).unwrap();
        controller.view.row = 0;
        controller.toggle_selection().unwrap();
        controller.view.row = 1;
        controller.toggle_selection().unwrap();
        assert_eq!(controller.selected_sheets.len(), 2);

        controller.execute(Command::DeleteTable, vec![]).unwrap();
        let remaining = controller.database.get_table_names().unwrap();
        assert_eq!(remaining, vec!["table_of_tables"]);
        assert!(controller.selected_sheets.is_empty());
        controller.execute(Command::Undo, vec![]).unwrap();
        let restored = controller.database.get_table_names().unwrap();
        assert!(restored.contains(&"t".into()));
        assert!(restored.contains(&"other".into()));
        assert_eq!(controller.selected_sheets.len(), 2);
        controller.execute(Command::Redo, vec![]).unwrap();
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
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
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
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
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
            .execute(Command::CombineTables, vec!["append".into()])
            .unwrap();
        let result = controller.view.table_name.clone();
        assert_eq!(controller.view.table_name, result);
        assert_eq!(controller.database.count_rows(&result).unwrap(), 4);
        assert_eq!(
            controller.database.get_headers(&result).unwrap(),
            vec!["id", "name", "extra"]
        );
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
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
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&result));
        controller.execute(Command::Redo, vec![]).unwrap();
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
        controller
            .execute(Command::ToggleMetadataTable, vec![])
            .unwrap();
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
            .execute(Command::CombineTables, vec!["full".into()])
            .unwrap();
        assert_eq!(
            controller
                .database
                .count_rows(&controller.view.table_name)
                .unwrap(),
            3
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
        controller
            .execute(Command::ConfigureAggregator, vec!["sum".into()])
            .unwrap();

        controller.execute(Command::Group, vec![]).unwrap();
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
        controller.execute(Command::Pivot, vec![]).unwrap();
        let headers = controller
            .database
            .get_headers(&controller.view.table_name)
            .unwrap();
        assert!(headers.contains(&"sum_amount_Jan".into()));
        assert!(headers.contains(&"sum_amount_Feb".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "t");
    }

    #[test]
    fn cell_edit_undoes_and_redoes_previous_typed_value() {
        let mut controller = controller();
        controller
            .execute(Command::Edit, vec!["changed".into()])
            .unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "changed"
        );

        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        controller.execute(Command::Redo, vec![]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "changed"
        );
    }

    #[test]
    fn bulk_edit_undoes_and_redoes_selected_rows() {
        let mut controller = controller();
        controller.selected_rows.extend([1, 2]);
        controller
            .execute(Command::BulkEdit, vec!["same".into()])
            .unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "same"
        );
        assert_eq!(
            controller.database.get_cell("t", 2, "name").unwrap(),
            "same"
        );
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        assert_eq!(controller.database.get_cell("t", 2, "name").unwrap(), "def");
        controller.execute(Command::Redo, vec![]).unwrap();
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
        controller.execute(Command::Copy, vec![]).unwrap();
        controller.log_command(Command::Copy, vec![], before);
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
        controller.replay_history(0).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
    }

    #[test]
    fn editing_the_row_id_is_rejected_without_history() {
        let mut controller = controller();
        controller.view.column = 0;
        assert!(controller
            .execute(Command::Edit, vec!["10".into()])
            .is_err());
        assert_eq!(controller.database.count_rows("t").unwrap(), 2);
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        assert!(controller.history.undo.is_empty());
    }

    #[test]
    fn rename_column_undoes_and_redoes() {
        let mut controller = controller();
        controller
            .execute(Command::RenameColumn, vec!["label".into()])
            .unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"label".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"label".into()));
    }

    #[test]
    fn rename_then_delete_uses_the_refreshed_column_name() {
        let mut controller = controller();
        controller
            .execute(Command::RenameColumn, vec!["label".into()])
            .unwrap();
        assert_eq!(controller.selected_column().unwrap(), "label");
        controller.execute(Command::DeleteColumn, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"label".into()));
    }

    #[test]
    fn generated_column_is_hidden_and_restored_by_history() {
        let mut controller = controller();
        controller.execute(Command::Copy, vec![]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name_copy").unwrap(),
            "abc"
        );
        controller.execute(Command::Undo, vec![]).unwrap();
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
        controller.execute(Command::Redo, vec![]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_copy".into()));
    }

    #[test]
    fn regex_transform_is_undoable_and_redoable() {
        let mut controller = controller();
        controller
            .execute(Command::RegexTransform, vec!["a".into()])
            .unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_tform".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name_tform".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
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

        controller.execute(Command::IntToText, vec![]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"TEXT_count".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"TEXT_count".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"TEXT_count".into()));

        controller.view.column = 2;
        controller.execute(Command::TextToInt, vec![]).unwrap();
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
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
        controller.execute(Command::TextToInt, vec![]).unwrap();
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
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_raw".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
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
        controller.execute(Command::Summary, vec![]).unwrap();
        assert!(controller.view.table_name.ends_with("_describe"));
        assert!(controller.view.headers.contains(&"column_name".into()));
        assert!(controller.view.headers.contains(&"null_count".into()));
        assert!(controller.view.headers.contains(&"distinct_count".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "t");
    }

    #[test]
    fn frequency_group_selection_propagates_to_source_and_clear_restores_baseline() {
        let mut controller = controller();
        controller.selected_rows.insert(2);
        controller.execute(Command::Frequency, vec![]).unwrap();
        let frequency = controller.view.table_name.clone();
        let row = controller
            .view
            .rows
            .iter()
            .position(|row| row.get(1) == Some(&DataItem::Text("abc".into())))
            .unwrap();
        controller.view.row = row;
        controller.toggle_selection().unwrap();
        assert!(controller.sheets["t"]
            .saved_view
            .as_ref()
            .unwrap()
            .selection
            .contains(&1));
        assert!(controller.sheets["t"]
            .saved_view
            .as_ref()
            .unwrap()
            .selection
            .contains(&2));

        controller.clear_selection().unwrap();
        assert!(!controller.sheets["t"]
            .saved_view
            .as_ref()
            .unwrap()
            .selection
            .contains(&1));
        assert!(controller.sheets["t"]
            .saved_view
            .as_ref()
            .unwrap()
            .selection
            .contains(&2));
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
        controller.execute(Command::Frequency, vec![]).unwrap();
        let frequency = controller.view.table_name.clone();
        let row = controller
            .view
            .rows
            .iter()
            .position(|row| row.get(1) == Some(&DataItem::Text("abc".into())))
            .unwrap();
        controller.view.row = row;
        controller.execute(Command::HistoryReplay, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "t");
        assert!(controller.selected_rows.contains(&1));
        assert!(controller.execute(Command::DeleteSelected, vec![]).is_err());
        assert_eq!(controller.database.count_rows("t").unwrap(), 2);
        controller.execute(Command::HistoryClose, vec![]).unwrap();
        assert_eq!(controller.view.table_name, frequency);
    }

    #[test]
    fn refresh_rebuilds_stale_frequency_sheet_after_source_edit() {
        let mut controller = controller();
        controller.execute(Command::Frequency, vec![]).unwrap();
        let frequency = controller.view.table_name.clone();

        controller.switch_sheet("t".into()).unwrap();
        controller.refresh_page(20).unwrap();
        controller.view.column = 1;
        controller.view.row = 0;
        controller
            .execute(Command::Edit, vec!["changed".into()])
            .unwrap();
        assert!(controller.sheets[&frequency].metadata.stale);
        let stale: i64 = controller
            .database
            .connection
            .query_row(
                "SELECT stale FROM table_of_tables WHERE table_name = ?1",
                [&frequency],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale, 1);

        controller.switch_sheet(frequency.clone()).unwrap();
        controller.execute(Command::RefreshDerived, vec![]).unwrap();
        assert!(!controller.sheets[&frequency].metadata.stale);
        let stale: i64 = controller
            .database
            .connection
            .query_row(
                "SELECT stale FROM table_of_tables WHERE table_name = ?1",
                [&frequency],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0);
        let changed_count: i64 = controller
            .database
            .connection
            .query_row(
                &format!(
                    "SELECT count FROM \"{}\" WHERE value = 'changed'",
                    frequency
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(changed_count, 1);
    }

    #[test]
    fn math_generated_column_is_undoable_and_redoable() {
        let mut controller = controller();
        controller
            .execute(Command::MathOperation, vec!["id + 10".into()])
            .unwrap();
        assert_eq!(
            controller
                .database
                .get_cell_item("t", 1, "math_expr")
                .unwrap(),
            DataItem::Integer(11)
        );
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"math_expr".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
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

        controller.execute(Command::TextToInt, vec![]).unwrap();
        let derived_column = controller.view.headers.len() - 1;
        controller.view.column = derived_column;
        controller.execute(Command::Sort, vec![]).unwrap();

        assert_eq!(
            controller.view.order_column.as_deref(),
            Some("INTEGER_count")
        );
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.view.order_column.as_deref(), Some("id"));
        assert!(controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));

        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"INTEGER_count".into()));
    }

    #[test]
    fn deleted_column_is_retained_and_restored() {
        let mut controller = controller();
        controller.execute(Command::DeleteColumn, vec![]).unwrap();
        assert!(!controller
            .database
            .get_headers("t")
            .unwrap()
            .contains(&"name".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        controller.execute(Command::Redo, vec![]).unwrap();
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

        assert!(controller.execute(Command::DeleteColumn, vec![]).is_err());
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
        controller
            .execute(Command::RegexFilter, vec!["a".into()])
            .unwrap();
        let filtered = controller.view.table_name.clone();
        assert_eq!(controller.database.count_rows(&filtered).unwrap(), 1);
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "t");
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&filtered));
        controller.execute(Command::Redo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, filtered);
        assert_eq!(controller.database.count_rows(&filtered).unwrap(), 1);
    }

    #[test]
    fn deleted_table_is_retained_and_restored() {
        let mut controller = controller();
        controller.execute(Command::DeleteTable, vec![]).unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        assert_eq!(controller.view.table_name, "t");
        controller.execute(Command::Redo, vec![]).unwrap();
        assert!(!controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
    }

    #[test]
    fn renamed_table_undoes_and_redoes() {
        let mut controller = controller();
        controller
            .execute(Command::RenameTable, vec!["renamed".into()])
            .unwrap();
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"renamed".into()));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "t");
        assert!(controller
            .database
            .get_table_names()
            .unwrap()
            .contains(&"t".into()));
        controller.execute(Command::Redo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "renamed");
    }

    #[test]
    fn new_edit_after_undo_discards_redo() {
        let mut controller = controller();
        controller
            .execute(Command::Edit, vec!["first".into()])
            .unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
        controller
            .execute(Command::Edit, vec!["second".into()])
            .unwrap();
        controller.execute(Command::Redo, vec![]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "second"
        );
    }

    #[test]
    fn configured_undo_redo_keys_reach_history_dispatch() {
        let mut controller = controller();
        controller
            .execute(Command::Edit, vec!["changed".into()])
            .unwrap();
        let undo = Command::from(crossterm::event::KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
        ));
        controller.execute(undo, vec![]).unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");

        let redo = Command::from(crossterm::event::KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        ));
        controller.execute(redo, vec![]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "changed"
        );
    }

    #[test]
    fn new_command_removes_hidden_redo_artifacts() {
        let mut controller = controller();
        controller.execute(Command::Copy, vec![]).unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
        assert!(raw_headers(&controller, "t")
            .iter()
            .any(|header| header.starts_with(INTERNAL_PREFIX)));
        controller
            .execute(Command::Edit, vec!["new value".into()])
            .unwrap();
        assert!(!raw_headers(&controller, "t")
            .iter()
            .any(|header| header.starts_with(INTERNAL_PREFIX)));
        assert!(controller.history.redo.is_empty());
    }

    #[test]
    fn successful_sql_clears_undo_history() {
        let mut controller = controller();
        controller
            .execute(Command::Edit, vec!["before sql".into()])
            .unwrap();
        controller
            .execute(
                Command::SqlQuery,
                vec!["UPDATE t SET name = 'from sql' WHERE id = 1;".into()],
            )
            .unwrap();
        assert!(controller.history.undo.is_empty());
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "from sql"
        );
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "from sql"
        );
    }

    #[test]
    fn failed_rename_does_not_enter_history() {
        let mut controller = controller();
        assert!(controller
            .execute(Command::RenameColumn, vec!["id".into()])
            .is_err());
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
                .execute(Command::Edit, vec![format!("value-{index}")])
                .unwrap();
        }
        assert_eq!(controller.history.undo.len(), HISTORY_LIMIT);
        for _ in 0..HISTORY_LIMIT {
            controller.execute(Command::Undo, vec![]).unwrap();
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
        let mut controller = Controller::new(database).unwrap();
        controller.view.table_name = "t".into();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        controller
            .execute(Command::Edit, vec!["not-a-number".into()])
            .unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(
            controller.database.get_cell_item("t", 1, "count").unwrap(),
            DataItem::Integer(42)
        );
    }

    #[test]
    fn sqlite_export_omits_hidden_history_artifacts() {
        let mut controller = controller();
        controller.execute(Command::DeleteTable, vec![]).unwrap();
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
        controller.execute(Command::Copy, vec![]).unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
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
        let mut controller = Controller::new(database).unwrap();
        controller.view.table_name = "t".into();
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        controller.execute(Command::DeleteTable, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "u");
        controller.get_headers_and_rows(10).unwrap();
        controller.view.column = 1;
        for index in 0..HISTORY_LIMIT {
            controller
                .execute(Command::Edit, vec![format!("value-{index}")])
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
            controller
                .execute(Command::Edit, vec!["changed".into()])
                .unwrap();
            controller
                .database
                .execute_batch("ALTER TABLE t DROP COLUMN name;")
                .unwrap();
            assert!(controller.execute(Command::Undo, vec![]).is_err());
            assert_eq!(controller.history.undo.len(), 1);
        }

        let mut redo_controller = controller();
        redo_controller
            .execute(Command::Edit, vec!["changed".into()])
            .unwrap();
        redo_controller.execute(Command::Undo, vec![]).unwrap();
        redo_controller
            .database
            .execute_batch("ALTER TABLE t DROP COLUMN name;")
            .unwrap();
        assert!(redo_controller.execute(Command::Redo, vec![]).is_err());
        assert_eq!(redo_controller.history.redo.len(), 1);
    }
    #[test]
    fn failed_commands_preserve_typed_errors_and_history() {
        let mut controller = controller();
        controller
            .execute(Command::Edit, vec!["changed".into()])
            .unwrap();
        let undo = controller.history.undo.len();
        let commands = controller.command_history.len();
        let error = controller
            .execute(
                Command::SqlQuery,
                vec!["UPDATE missing SET name='x'".into()],
            )
            .unwrap_err();
        assert!(matches!(error, AppError::Sqlite(_)));
        assert_eq!(controller.history.undo.len(), undo);
        assert_eq!(controller.command_history.len(), commands);
        assert!(matches!(
            controller.execute(Command::RegexFilter, vec!["[".into()]),
            Err(AppError::Regex(_))
        ));
        assert_eq!(controller.history.undo.len(), undo);
        assert_eq!(controller.command_history.len(), commands);
    }
    #[test]
    fn committed_edit_survives_sheet_list_failure_and_can_be_undone() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let mut controller = controller();
        controller
            .database
            .connection
            .authorizer(Some(|ctx: AuthContext<'_>| {
                if matches!(
                    ctx.action,
                    AuthAction::Insert {
                        table_name: "table_of_tables"
                    }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
        let (execution, snapshot) =
            controller.execute_and_snapshot(Command::Edit, vec!["committed".into()], None);
        execution.unwrap();
        assert!(snapshot.is_err());
        assert_eq!(controller.history.undo.len(), 1);
        assert_eq!(controller.command_history.len(), 1);
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "committed"
        );
        controller
            .database
            .connection
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.database.get_cell("t", 1, "name").unwrap(), "abc");
        controller.replay_history(0).unwrap();
        assert_eq!(controller.history.undo.len(), 1);
        assert_eq!(controller.command_history.len(), 1);
        assert_eq!(
            controller.database.get_cell("t", 1, "name").unwrap(),
            "committed"
        );
    }

    #[test]
    fn registry_rename_delete_undo_redo_preserves_references_and_saved_state() {
        let mut controller = controller();
        controller
            .database
            .execute_batch("ALTER TABLE t ADD COLUMN count INTEGER")
            .unwrap();
        controller.get_headers_and_rows(100).unwrap();
        controller.execute(Command::ToggleJoinKey, vec![]).unwrap();
        controller
            .execute(Command::ConfigureAggregator, vec!["count".into()])
            .unwrap();
        controller.selected_rows.insert(2);
        controller.execute(Command::Frequency, vec![]).unwrap();
        let frequency = controller.view.table_name.clone();
        let membership = controller.sheets[&frequency]
            .metadata
            .frequency
            .as_ref()
            .unwrap()
            .1
            .clone();
        controller.switch_sheet("t".into()).unwrap();
        controller.refresh_page(100).unwrap();
        controller.view.column = 2;
        controller.execute(Command::HideColumn, vec![]).unwrap();
        controller.view.column = 1;
        controller.switch_sheet(frequency.clone()).unwrap();
        controller.switch_sheet("t".into()).unwrap();
        controller.selected_sheets.insert("t".into());
        controller.previous_sheet = Some("t".into());
        controller.view.table_before_metadata = Some("t".into());
        controller
            .execute(Command::RenameTable, vec!["renamed".into()])
            .unwrap();
        assert_eq!(controller.previous_sheet.as_deref(), Some("renamed"));
        assert_eq!(
            controller.view.table_before_metadata.as_deref(),
            Some("renamed")
        );
        assert!(controller.selected_sheets.contains("renamed"));
        assert_eq!(
            Controller::derived_sources(
                controller.sheets[&frequency]
                    .metadata
                    .derived
                    .as_ref()
                    .unwrap()
            ),
            ["renamed"]
        );
        assert_eq!(
            controller.sheets[&frequency]
                .metadata
                .frequency
                .as_ref()
                .unwrap()
                .0,
            "renamed"
        );
        assert_eq!(controller.sheets["renamed"].metadata.keys, ["name"]);
        assert!(controller.sheets["renamed"].layout.hidden.contains("count"));
        assert!(controller.sheets["renamed"]
            .saved_view
            .as_ref()
            .unwrap()
            .selection
            .contains(&2));
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(
            controller.sheets[&frequency]
                .metadata
                .frequency
                .as_ref()
                .unwrap()
                .0,
            "t"
        );
        assert!(controller.sheets["t"].layout.hidden.contains("count"));
        controller.execute(Command::Redo, vec![]).unwrap();
        controller.execute(Command::DeleteTable, vec![]).unwrap();
        assert!(!controller.sheets["renamed"].present);
        assert_eq!(controller.sheets["renamed"].metadata.keys, ["name"]);
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.view.table_name, "renamed");
        assert!(controller.selected_rows.contains(&2));
        assert!(controller.sheets["renamed"].layout.hidden.contains("count"));
        assert_eq!(controller.sheets["renamed"].metadata.aggregators.len(), 1);
        controller.execute(Command::Redo, vec![]).unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
        controller.switch_sheet(frequency.clone()).unwrap();
        controller.refresh_page(100).unwrap();
        controller.execute(Command::DeleteTable, vec![]).unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(
            controller.sheets[&frequency]
                .metadata
                .frequency
                .as_ref()
                .unwrap()
                .1,
            membership
        );
        assert!(controller.sheets[&frequency]
            .metadata
            .frequency_baseline
            .contains(&2));
        assert!(!controller
            .database
            .frequency_group_members(&membership, 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn deleted_registry_entry_expires_with_its_undo_record() {
        let mut controller = controller();
        controller.execute(Command::DeleteTable, vec![]).unwrap();
        assert!(controller.sheets.contains_key("t"));
        for _ in 0..HISTORY_LIMIT {
            controller.execute(Command::Sort, vec![]).unwrap();
        }
        assert!(!controller.sheets.contains_key("t"));
    }

    #[test]
    fn read_only_context_rejects_input_and_replay_without_logging() {
        let mut controller = controller();
        controller
            .execute(Command::Edit, vec!["changed".into()])
            .unwrap();
        let undo = controller.history.undo.len();
        let logged = controller.command_history.len();
        controller.drilldown_return_sheet = Some("frequency".into());
        assert!(controller
            .execute(Command::Edit, vec!["bad".into()])
            .is_err());
        assert!(controller.replay_history(0).is_err());
        assert_eq!(controller.history.undo.len(), undo);
        assert_eq!(controller.command_history.len(), logged);
    }
    #[test]
    fn column_rename_updates_derived_specs_markers_and_saved_sort_through_history() {
        let mut controller = controller();
        controller.execute(Command::ToggleJoinKey, vec![]).unwrap();
        controller
            .execute(Command::ConfigureAggregator, vec!["count".into()])
            .unwrap();
        controller.view.order_column = Some("name".into());
        controller.execute(Command::Frequency, vec![]).unwrap();
        let frequency = controller.view.table_name.clone();
        controller.switch_sheet("t".into()).unwrap();
        controller.refresh_page(100).unwrap();
        controller
            .execute(Command::RenameColumn, vec!["label".into()])
            .unwrap();
        assert_eq!(controller.sheets["t"].metadata.keys, ["label"]);
        assert_eq!(controller.sheets["t"].metadata.aggregators[0].0, "label");
        assert_eq!(
            controller.sheets["t"]
                .saved_view
                .as_ref()
                .unwrap()
                .order_column
                .as_deref(),
            Some("label")
        );
        assert!(
            matches!(&controller.sheets[&frequency].metadata.derived, Some(DerivedSpec::Frequency { column, .. }) if column == "label")
        );
        controller.execute(Command::Undo, vec![]).unwrap();
        assert_eq!(controller.sheets["t"].metadata.keys, ["name"]);
        assert_eq!(
            controller.sheets["t"]
                .saved_view
                .as_ref()
                .unwrap()
                .order_column
                .as_deref(),
            Some("name")
        );
        controller.execute(Command::Redo, vec![]).unwrap();
        controller.switch_sheet(frequency).unwrap();
        controller.execute(Command::RefreshDerived, vec![]).unwrap();
        let source_column = controller
            .view
            .headers
            .iter()
            .position(|header| header == "source_column")
            .unwrap();
        assert_eq!(
            controller.view.rows[0][controller.view.column_indexes[source_column]],
            DataItem::Text("label".into())
        );
    }

    #[test]
    fn history_does_not_restore_other_sheets_saved_navigation() {
        let mut controller = controller();
        controller.database.execute_batch("CREATE TABLE other(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO other VALUES(1,'a'),(2,'b')").unwrap();
        controller
            .execute(Command::Edit, vec!["edited".into()])
            .unwrap();
        controller.switch_sheet("other".into()).unwrap();
        controller.view.row = 1;
        controller.selected_rows.insert(2);
        controller.switch_sheet("t".into()).unwrap();
        controller.execute(Command::Undo, vec![]).unwrap();
        controller.switch_sheet("other".into()).unwrap();
        assert_eq!(controller.view.row, 1);
        assert!(controller.selected_rows.contains(&2));
    }
}
