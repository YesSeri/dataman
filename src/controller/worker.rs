//! Terminal state never borrows SQLite. One worker serializes all database access.
use super::selection::Selection;
use super::{
    command::{Command, PreviousCommand},
    controller_impl::{Controller, UiState, ViewState},
    input::{self, InputMode, StateMachine},
};
use crate::{
    error::{AppError, AppResult},
    navigation::Direction,
    tui::TUI,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationId(u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewGeneration(u64);

#[derive(Debug, Clone)]
pub(crate) struct HistoryLine {
    pub(crate) command: Command,
    pub(crate) table: String,
    pub(crate) inputs: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RenderState {
    pub(crate) ui: UiState,
    pub(crate) view: ViewState,
    pub(crate) selected_rows: Selection,
    pub(crate) selected_sheets: HashSet<String>,
    pub(crate) marked_join_keys: Vec<String>,
    pub(crate) command_history: Vec<HistoryLine>,
    pub(crate) total_rows: u32,
    pub(crate) busy: bool,
    pub(crate) column_widths: Vec<u16>,
    pub(crate) busy_phase: &'static str,
    pub(crate) busy_frame: usize,
}
impl RenderState {
    fn loading() -> Self {
        Self {
            ui: UiState {
                input: String::new(),
                character_index: 0,
                last_command: PreviousCommand::new(
                    Command::None,
                    Some("Loading… Esc cancels".into()),
                ),
                queued_command: None,
                input_mode_state_machine: StateMachine::new(),
                history_view: false,
                history_cursor: 0,
            },
            view: ViewState {
                table_name: String::new(),
                column: 0,
                row: 0,
                row_offset: 0,
                order_column: None,
                ascending: true,
                headers: vec![],
                rows: Arc::default(),
                column_indexes: vec![],
                table_before_metadata: None,
            },
            selected_rows: Selection::default(),
            selected_sheets: HashSet::new(),
            marked_join_keys: vec![],
            command_history: vec![],
            total_rows: 0,
            busy: true,
            column_widths: vec![],
            busy_phase: "Loading",
            busy_frame: 0,
        }
    }
    pub(crate) fn from_controller(controller: &Controller) -> AppResult<Self> {
        Ok(Self {
            ui: controller.ui.clone(),
            view: controller.view.clone(),
            selected_rows: controller.selected_rows.clone(),
            selected_sheets: controller.selected_sheets.clone(),
            marked_join_keys: controller
                .join_keys
                .get(&controller.view.table_name)
                .cloned()
                .unwrap_or_default(),
            command_history: controller
                .command_history
                .iter()
                .map(|entry| HistoryLine {
                    command: entry.command.clone(),
                    table: entry.table.clone(),
                    inputs: entry.inputs.clone(),
                })
                .collect(),
            total_rows: controller
                .database
                .count_rows(&controller.view.table_name)?,
            busy: false,
            busy_phase: "Working",
            busy_frame: 0,
            column_widths: crate::tui::column_widths(
                &controller.view.headers,
                &controller.view.rows,
                &controller.view.column_indexes,
            ),
        })
    }
}

struct ViewContext {
    row: usize,
    column: usize,
    selected_rows: Selection,
    history_cursor: usize,
}
struct Operation {
    id: OperationId,
    generation: ViewGeneration,
    command: Command,
    inputs: Option<Vec<String>>,
    context: ViewContext,
    height: u32,
    cancellation: Arc<AtomicBool>,
}
enum DatabaseRequest {
    Execute(Operation),
}
enum DatabaseResponse {
    Ready(RenderState),
    Completed {
        id: OperationId,
        generation: ViewGeneration,
        state: RenderState,
        error: Option<AppError>,
    },
    OperationFailed {
        id: OperationId,
        generation: ViewGeneration,
        error: AppError,
    },
    Progress {
        id: OperationId,
        phase: &'static str,
    },
    Failed(AppError),
}

fn serve(
    mut controller: Controller,
    requests: Receiver<DatabaseRequest>,
    responses: SyncSender<DatabaseResponse>,
) -> AppResult<()> {
    let initial = controller
        .refresh_page(100)
        .and_then(|()| RenderState::from_controller(&controller));
    match initial {
        Ok(state) => {
            if responses.send(DatabaseResponse::Ready(state)).is_err() {
                return controller.shutdown();
            }
        }
        Err(error) => {
            let _ = responses.send(DatabaseResponse::Failed(
                error.context("Loading initial page"),
            ));
            return controller.shutdown();
        }
    }
    while let Ok(request) = requests.recv() {
        let DatabaseRequest::Execute(operation) = request;
        controller.database.cancellation = operation.cancellation.clone();
        crate::model::database::Database::install_cancellation(
            &controller.database.connection,
            operation.cancellation.clone(),
        );
        controller.view.row = operation.context.row;
        controller.view.column = operation.context.column;
        controller.selected_rows = operation.context.selected_rows;
        controller.ui.history_cursor = operation.context.history_cursor;
        controller.ui.history_view = operation.command == Command::HistoryReplay;
        controller.table_height = operation.height;
        controller.defer_refresh = true;
        controller.ui.last_command = PreviousCommand::new(operation.command.clone(), None);
        if matches!(
            responses.try_send(DatabaseResponse::Progress {
                id: operation.id,
                phase: "Working",
            }),
            Err(mpsc::TrySendError::Disconnected(_))
        ) {
            break;
        }
        let result = if operation.cancellation.load(Ordering::Relaxed) {
            Err(AppError::Cancelled)
        } else if let Some(inputs) = operation.inputs {
            controller.execute_inputs(operation.command.clone(), inputs)
        } else {
            controller.dispatch(operation.command.clone(), operation.height)
        };
        let error = result.err().map(|error| {
            error
                .normalize_cancelled(operation.cancellation.load(Ordering::Relaxed))
                .context(format!(
                    "Executing {} on '{}'",
                    operation.command, controller.view.table_name
                ))
        });
        // Cancellation belongs to execution, not the refresh following a committed command.
        operation.cancellation.store(false, Ordering::Relaxed);
        controller.defer_refresh = false;
        if matches!(
            responses.try_send(DatabaseResponse::Progress {
                id: operation.id,
                phase: "Refreshing page",
            }),
            Err(mpsc::TrySendError::Disconnected(_))
        ) {
            break;
        }
        let snapshot = controller
            .refresh_page(100)
            .and_then(|()| RenderState::from_controller(&controller));
        let response = match snapshot {
            Ok(state) => DatabaseResponse::Completed {
                id: operation.id,
                generation: operation.generation,
                state,
                error,
            },
            Err(refresh) => {
                let refresh = refresh.context(if error.is_some() {
                    "Loading page after failed command"
                } else {
                    "Command completed; page could not be loaded"
                });
                let error = match error {
                    Some(error) => error.with_cleanup(refresh),
                    None => refresh,
                };
                DatabaseResponse::OperationFailed {
                    id: operation.id,
                    generation: operation.generation,
                    error,
                }
            }
        };
        if responses.send(response).is_err() {
            break;
        }
    }
    controller
        .database
        .cancellation
        .store(false, Ordering::Relaxed);
    controller.shutdown()
}

pub(super) fn run(
    load: impl FnOnce(Arc<AtomicBool>) -> AppResult<Controller> + Send + 'static,
    mut tui: TUI,
) -> AppResult<()> {
    let height = match TUI::get_table_height() {
        Ok(height) => height.min(100),
        Err(error) => {
            return crate::error::finish(
                Err(error.context("Reading terminal size")),
                tui.shutdown(),
            )
        }
    };
    let (requests, worker_requests) = mpsc::sync_channel(1);
    let (worker_responses, responses) = mpsc::sync_channel(1);
    let startup_cancellation = Arc::new(AtomicBool::new(false));
    let startup_token = startup_cancellation.clone();
    let worker = thread::Builder::new()
        .name("dataman-database".into())
        .spawn(move || match load(startup_token) {
            Ok(controller) => serve(controller, worker_requests, worker_responses),
            Err(error) => {
                let _ = worker_responses.send(DatabaseResponse::Failed(error));
                Ok(())
            }
        });
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            return crate::error::finish(
                Err(AppError::from(error).context("Starting database worker")),
                tui.shutdown(),
            )
        }
    };
    let mut frontend = Frontend {
        state: RenderState::loading(),
        requests,
        responses,
        next_id: 0,
        height,
        active: Some((OperationId(0), ViewGeneration(0), startup_cancellation)),
        deferred: None,
        generation: ViewGeneration(0),
        moved_while_busy: false,
        dirty: true,
        quitting: false,
    };
    let result = frontend.event_loop(&mut tui);
    if let Some((_, _, token)) = &frontend.active {
        token.store(true, Ordering::Relaxed);
    }
    // Drop the sender instead of trying to enqueue shutdown behind an active request.
    drop(frontend);
    let joined = worker
        .join()
        .map_err(|_| AppError::from("Database worker failed"))
        .and_then(|result| result);
    let shutdown = tui.shutdown();
    crate::error::finish(crate::error::finish(result, joined), shutdown)
}

struct Frontend {
    state: RenderState,
    requests: SyncSender<DatabaseRequest>,
    responses: Receiver<DatabaseResponse>,
    next_id: u64,
    height: u32,
    generation: ViewGeneration,
    active: Option<(OperationId, ViewGeneration, Arc<AtomicBool>)>,
    deferred: Option<Command>,
    moved_while_busy: bool,
    dirty: bool,
    quitting: bool,
}
impl Frontend {
    fn event_loop(&mut self, tui: &mut TUI) -> AppResult<()> {
        let mut animation = Instant::now();
        loop {
            loop {
                match self.responses.try_recv() {
                    Ok(DatabaseResponse::Ready(state)) => {
                        self.state = state;
                        self.active = None;
                        self.dirty = true;
                    }
                    Ok(DatabaseResponse::Completed {
                        id,
                        generation,
                        mut state,
                        error,
                    }) => {
                        if self
                            .active
                            .as_ref()
                            .is_some_and(|(active, _, _)| *active == id)
                            && generation == self.generation
                        {
                            if let Some(error) = error {
                                state.ui.last_command = PreviousCommand::new(
                                    Command::IllegalOperation,
                                    Some(error.present()),
                                );
                            }
                        }
                        self.complete(id, generation, state);
                    }
                    Ok(DatabaseResponse::OperationFailed {
                        id,
                        generation,
                        error,
                    }) => {
                        self.fail(id, generation, error);
                    }
                    Ok(DatabaseResponse::Progress { id, phase }) => {
                        if self
                            .active
                            .as_ref()
                            .is_some_and(|(active, _, _)| *active == id)
                        {
                            self.state.busy_phase = phase;
                            self.dirty = true;
                        }
                    }
                    Ok(DatabaseResponse::Failed(error)) => {
                        if self.quitting {
                            return Ok(());
                        }
                        return Err(error);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if self.quitting {
                            return Ok(());
                        }
                        return Err(AppError::from("Database worker disconnected"));
                    }
                }
            }
            if self.quitting && self.active.is_none() {
                return Ok(());
            }
            if self.active.is_none() && !self.quitting {
                if let Some(command) = self.deferred.take() {
                    self.submit(command, None)?;
                }
            }
            self.state.busy = self.active.is_some();
            if self.dirty || (self.state.busy && animation.elapsed() >= Duration::from_millis(100))
            {
                self.state.busy_frame = self.state.busy_frame.wrapping_add(1);
                tui.draw(&self.state)?;
                self.dirty = false;
                animation = Instant::now();
            }
            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Resize(_, height) => {
                        self.height = u32::from(height.saturating_sub(4)).min(100);
                        self.state.view.row = self
                            .state
                            .view
                            .row
                            .min(self.height.saturating_sub(1) as usize);
                        self.dirty = true;
                    }
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.key(key)?,
                    _ => {}
                }
            }
        }
    }
    fn fail(&mut self, id: OperationId, generation: ViewGeneration, error: AppError) {
        if !self
            .active
            .as_ref()
            .is_some_and(|(active, _, _)| *active == id)
        {
            return;
        }
        self.active = None;
        self.state.busy = false;
        if generation == self.generation {
            self.state.ui.last_command =
                PreviousCommand::new(Command::IllegalOperation, Some(error.present()));
        }
        self.dirty = true;
    }
    fn complete(&mut self, id: OperationId, generation: ViewGeneration, mut state: RenderState) {
        if !self
            .active
            .as_ref()
            .is_some_and(|(active, _, _)| *active == id)
        {
            return;
        }
        self.active = None;
        self.state.busy = false;
        state.busy = false;
        if generation == self.generation {
            if self.moved_while_busy
                && state.view.table_name == self.state.view.table_name
                && state.view.row_offset == self.state.view.row_offset
            {
                state.view.row = self
                    .state
                    .view
                    .row
                    .min(state.view.rows.len().saturating_sub(1));
                state.view.column = self
                    .state
                    .view
                    .column
                    .min(state.view.headers.len().saturating_sub(1));
            }
            state.view.row = state.view.row.min(self.height.saturating_sub(1) as usize);
            self.state = state;
        }
        self.dirty = true;
    }
    fn submit(&mut self, command: Command, inputs: Option<Vec<String>>) -> AppResult<()> {
        self.next_id += 1;
        self.generation.0 += 1;
        let id = OperationId(self.next_id);
        let cancellation = Arc::new(AtomicBool::new(false));
        let operation = Operation {
            id,
            generation: self.generation,
            command,
            inputs,
            context: ViewContext {
                row: self.state.view.row,
                column: self.state.view.column,
                selected_rows: self.state.selected_rows.clone(),
                history_cursor: self.state.ui.history_cursor,
            },
            height: self.height,
            cancellation: cancellation.clone(),
        };
        self.requests
            .try_send(DatabaseRequest::Execute(operation))
            .map_err(|_| AppError::from("Database worker unavailable"))?;
        self.active = Some((id, self.generation, cancellation));
        self.moved_while_busy = false;
        self.dirty = true;
        Ok(())
    }
    fn key(&mut self, key: KeyEvent) -> AppResult<()> {
        self.dirty = true;
        if self.state.ui.input_mode_state_machine.get_state() == InputMode::Editing {
            return self.edit(key);
        }
        let command = Command::from(key);
        if command == Command::Quit {
            self.quitting = true;
            if let Some((_, _, token)) = &self.active {
                token.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }
        if self.active.is_some() {
            if key.code == KeyCode::Esc {
                if let Some((_, _, token)) = &self.active {
                    token.store(true, Ordering::Relaxed);
                }
                self.state.ui.last_command.message = Some("Cancelling…".into());
            } else if let Command::Move(direction) = command {
                if !self.move_cached(direction) {
                    self.deferred = Some(Command::Move(direction));
                }
                self.moved_while_busy = true;
            } else if matches!(
                command,
                Command::NextTable | Command::PrevTable | Command::PreviousSheet | Command::Sort
            ) {
                self.deferred = Some(command);
            } else if command != Command::None {
                self.state.ui.last_command.message = Some("Busy; Esc cancels".into());
            }
            return Ok(());
        }
        if self.state.ui.history_view {
            match command {
                Command::History | Command::HistoryClose => self.state.ui.history_view = false,
                Command::Move(Direction::Up) => {
                    self.state.ui.history_cursor = self.state.ui.history_cursor.saturating_sub(1)
                }
                Command::Move(Direction::Down) => {
                    self.state.ui.history_cursor = (self.state.ui.history_cursor + 1)
                        .min(self.state.command_history.len().saturating_sub(1))
                }
                Command::HistoryReplay => self.submit(command, None)?,
                _ => {}
            }
            return Ok(());
        }
        match command {
            Command::Move(direction) => {
                if !self.move_cached(direction) {
                    self.submit(command, None)?;
                }
            }
            Command::ToggleSelection => {
                if let Some(crate::model::datarow::DataItem::Integer(id)) = self
                    .state
                    .view
                    .rows
                    .get(self.state.view.row)
                    .and_then(|row| row.get(*self.state.view.column_indexes.first()?))
                {
                    if !self.state.selected_rows.insert(*id) {
                        self.state.selected_rows.remove(id);
                    }
                }
            }
            Command::ClearSelection => self.state.selected_rows.clear(),
            Command::History => {
                self.state.ui.history_view = true;
                self.state.ui.history_cursor = self.state.command_history.len().saturating_sub(1);
            }
            Command::None | Command::IllegalOperation | Command::HistoryClose => {}
            command if command.needs_input() => {
                self.state.ui.queued_command =
                    Some(super::command::QueuedCommand::new(command.clone()));
                self.state.ui.last_command = PreviousCommand::new(command.clone(), None);
                if command == Command::RenameTable {
                    self.state.ui.input = self.state.view.table_name.clone();
                    self.state.ui.character_index = self.state.ui.input.chars().count();
                }
                self.state
                    .ui
                    .input_mode_state_machine
                    .transition(input::Event::StartEditing)?;
            }
            command => self.submit(command, None)?,
        }
        Ok(())
    }
    fn move_cached(&mut self, direction: Direction) -> bool {
        let height = self.height.max(1) as usize;
        let view = &mut self.state.view;
        match direction {
            Direction::Left => {
                if !view.headers.is_empty() {
                    view.column = (view.column + view.headers.len() - 1) % view.headers.len();
                }
            }
            Direction::Right => {
                if !view.headers.is_empty() {
                    view.column = (view.column + 1) % view.headers.len();
                }
            }
            Direction::Up if view.row > 0 => view.row -= 1,
            Direction::Up => return view.row_offset == 0,
            Direction::Down if view.row + 1 < height.min(view.rows.len()) => view.row += 1,
            Direction::Down => {
                return u64::from(view.row_offset) + view.row as u64 + 1
                    >= u64::from(self.state.total_rows)
            }
        }
        true
    }
    fn edit(&mut self, key: KeyEvent) -> AppResult<()> {
        let ui = &mut self.state.ui;
        let byte = ui
            .input
            .char_indices()
            .nth(ui.character_index)
            .map(|(i, _)| i)
            .unwrap_or(ui.input.len());
        match key.code {
            KeyCode::Esc | KeyCode::Char('c')
                if key.code == KeyCode::Esc || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                ui.input.clear();
                ui.character_index = 0;
                ui.queued_command = None;
                ui.input_mode_state_machine
                    .transition(input::Event::AbortEditing)?;
                ui.input_mode_state_machine
                    .transition(input::Event::Reset)?;
            }
            KeyCode::Char(character) => {
                ui.input.insert(byte, character);
                ui.character_index += 1;
            }
            KeyCode::Backspace if ui.character_index > 0 => {
                let previous = ui
                    .input
                    .char_indices()
                    .nth(ui.character_index - 1)
                    .map(|(index, _)| index)
                    .ok_or_else(|| AppError::invalid_input("Input cursor is out of bounds"))?;
                ui.input.replace_range(previous..byte, "");
                ui.character_index -= 1;
            }
            KeyCode::Left => ui.character_index = ui.character_index.saturating_sub(1),
            KeyCode::Right => {
                ui.character_index = (ui.character_index + 1).min(ui.input.chars().count())
            }
            KeyCode::Tab => match TUI::get_external_editor_input(&ui.input) {
                Ok(input) => {
                    ui.character_index = input.chars().count();
                    ui.input = input;
                }
                Err(error) => ui.last_command.message = Some(error.present()),
            },
            KeyCode::Enter => {
                let queued = ui
                    .queued_command
                    .as_mut()
                    .ok_or_else(|| AppError::from("Missing command"))?;
                let template = queued.command == Command::RegexTransform
                    && queued.inputs.is_empty()
                    && regex::Regex::new(&ui.input).is_ok_and(|regex| regex.captures_len() > 1);
                queued.inputs.push(std::mem::take(&mut ui.input));
                ui.character_index = 0;
                if !template {
                    let queued = ui
                        .queued_command
                        .take()
                        .ok_or_else(|| AppError::from("Missing command"))?;
                    ui.input_mode_state_machine
                        .transition(input::Event::FinishEditing)?;
                    ui.input_mode_state_machine
                        .transition(input::Event::Reset)?;
                    self.submit(queued.command, Some(queued.inputs))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{database::Database, datarow::DataItem};
    use rusqlite::{functions::FunctionFlags, Connection};
    use std::sync::Barrier;

    fn controller() -> Controller {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES(1,'one'),(2,'two');").unwrap();
        Controller::new(Database::new(connection).unwrap()).unwrap()
    }
    fn operation(
        id: u64,
        command: Command,
        inputs: Option<Vec<String>>,
        cancellation: Arc<AtomicBool>,
    ) -> DatabaseRequest {
        DatabaseRequest::Execute(Operation {
            id: OperationId(id),
            generation: ViewGeneration(id),
            command,
            inputs,
            context: ViewContext {
                row: 0,
                column: 1,
                selected_rows: Selection::default(),
                history_cursor: 0,
            },
            height: 20,
            cancellation,
        })
    }
    fn receive(receiver: &Receiver<DatabaseResponse>) -> DatabaseResponse {
        loop {
            let response = receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("worker did not respond");
            if !matches!(response, DatabaseResponse::Progress { .. }) {
                return response;
            }
        }
    }
    #[test]
    fn worker_cancels_sql_rolls_back_and_accepts_next_command() {
        let controller = controller();
        let barrier = Arc::new(Barrier::new(2));
        let wait = barrier.clone();
        let (entered, started) = mpsc::sync_channel(1);
        let mut first = true;
        controller
            .database
            .connection
            .create_scalar_function("pause", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
                if first {
                    first = false;
                    entered.send(()).unwrap();
                    wait.wait();
                }
                ctx.get::<String>(0)
            })
            .unwrap();
        let (send, requests) = mpsc::sync_channel(1);
        let (responses, receive_results) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || serve(controller, requests, responses));
        assert!(matches!(
            receive(&receive_results),
            DatabaseResponse::Ready(_)
        ));
        let cancellation = Arc::new(AtomicBool::new(false));
        send.send(operation(
            1,
            Command::SqlQuery,
            Some(vec!["UPDATE t SET name=pause('changed');".into()]),
            cancellation.clone(),
        ))
        .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        cancellation.store(true, Ordering::Relaxed);
        barrier.wait();
        let DatabaseResponse::Completed { state, error, .. } = receive(&receive_results) else {
            panic!("missing completion");
        };
        assert!(error.unwrap().is_cancelled());
        assert_eq!(state.view.rows[0][1], DataItem::Text("one".into()));
        send.send(operation(
            2,
            Command::Edit,
            Some(vec!["next".into()]),
            Arc::new(AtomicBool::new(false)),
        ))
        .unwrap();
        let DatabaseResponse::Completed { state, .. } = receive(&receive_results) else {
            panic!("missing completion");
        };
        assert_eq!(state.view.rows[0][1], DataItem::Text("next".into()));
        send.send(operation(
            3,
            Command::Undo,
            None,
            Arc::new(AtomicBool::new(false)),
        ))
        .unwrap();
        let DatabaseResponse::Completed { state, .. } = receive(&receive_results) else {
            panic!("missing undo");
        };
        assert_eq!(state.view.rows[0][1], DataItem::Text("one".into()));
        drop(send);
        worker.join().unwrap().unwrap();
    }
    fn frontend() -> (Frontend, Receiver<DatabaseRequest>) {
        let (requests, receiver) = mpsc::sync_channel(1);
        let (_, responses) = mpsc::sync_channel(1);
        let mut controller = controller();
        controller.refresh_page(100).unwrap();
        (
            Frontend {
                state: RenderState::from_controller(&controller).unwrap(),
                requests,
                responses,
                next_id: 1,
                height: 20,
                generation: ViewGeneration(1),
                active: Some((
                    OperationId(1),
                    ViewGeneration(1),
                    Arc::new(AtomicBool::new(false)),
                )),
                deferred: None,
                moved_while_busy: false,
                dirty: false,
                quitting: false,
            },
            receiver,
        )
    }
    #[test]
    fn stale_responses_cannot_replace_active_view() {
        let (mut frontend, _) = frontend();
        let mut stale = frontend.state.clone();
        stale.view.table_name = "wrong".into();
        frontend.complete(OperationId(0), ViewGeneration(0), stale);
        assert_eq!(frontend.state.view.table_name, "t");
        assert!(frontend.active.is_some());
        let current = frontend.state.clone();
        frontend.complete(OperationId(1), ViewGeneration(1), current);
        assert!(frontend.active.is_none());
    }
    #[test]
    fn busy_navigation_coalesces_and_escape_and_quit_cancel() {
        let (mut frontend, receiver) = frontend();
        for command in ['s', 's', 's'] {
            frontend
                .key(KeyEvent::new(KeyCode::Char(command), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(frontend.deferred, Some(Command::Sort));
        assert!(receiver.try_recv().is_err());
        frontend
            .key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(frontend.state.view.column, 1);
        frontend
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(frontend.active.as_ref().unwrap().2.load(Ordering::Relaxed));
        frontend
            .key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(frontend.quitting);
    }
    #[test]
    fn cached_navigation_is_preserved_when_operation_completes() {
        let (mut frontend, _) = frontend();
        let response = frontend.state.clone();
        frontend
            .key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        frontend.complete(OperationId(1), ViewGeneration(1), response);
        assert_eq!(frontend.state.view.column, 1);
    }
    #[test]
    fn editing_unicode_and_regex_templates_stays_on_the_ui_thread() {
        let (mut frontend, receiver) = frontend();
        frontend.active = None;
        frontend
            .key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        for character in ['α', '🙂'] {
            frontend
                .key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        frontend
            .key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        frontend
            .key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(frontend.state.ui.input, "🙂");
        assert_eq!(frontend.state.ui.character_index, 0);
        frontend
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        frontend
            .key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE))
            .unwrap();
        for character in "(a)".chars() {
            frontend
                .key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        frontend
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            frontend.state.ui.input_mode_state_machine.get_state(),
            InputMode::Editing
        );
        assert!(receiver.try_recv().is_err());
        for character in "$1$1".chars() {
            frontend
                .key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        frontend
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let DatabaseRequest::Execute(operation) = receiver.try_recv().unwrap();
        assert_eq!(operation.command, Command::RegexTransform);
        assert_eq!(operation.inputs.unwrap(), ["(a)", "$1$1"]);
        frontend.active = None;
        frontend
            .key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE))
            .unwrap();
        for character in ".+".chars() {
            frontend
                .key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        frontend
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let DatabaseRequest::Execute(operation) = receiver.try_recv().unwrap();
        assert_eq!(operation.command, Command::RegexTransform);
        assert_eq!(operation.inputs.unwrap(), [".+"]);
    }

    #[test]
    fn worker_shutdown_closes_and_removes_session_storage() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("data.csv");
        std::fs::write(&input, "name\none\n").unwrap();
        let database = Database::try_from(vec![input]).unwrap();
        let session = database
            .session_directory
            .as_ref()
            .unwrap()
            .path()
            .to_owned();
        let (sender, requests) = mpsc::sync_channel(1);
        let (responses, receiver) = mpsc::sync_channel(1);
        let worker =
            thread::spawn(move || serve(Controller::new(database).unwrap(), requests, responses));
        assert!(matches!(receive(&receiver), DatabaseResponse::Ready(_)));
        drop(sender);
        worker.join().unwrap().unwrap();
        assert!(!session.exists());
    }
    #[test]
    fn snapshot_count_errors_are_fallible() {
        use rusqlite::hooks::{AuthContext, Authorization};
        let mut controller = controller();
        controller.refresh_page(100).unwrap();
        controller
            .database
            .connection
            .authorizer(Some(|_: AuthContext<'_>| Authorization::Deny));
        assert!(RenderState::from_controller(&controller).is_err());
    }

    #[test]
    fn snapshot_failure_keeps_previous_page_and_clears_busy_state() {
        let (mut frontend, _requests) = frontend();
        let rows = frontend.state.view.rows.clone();
        frontend.state.busy = true;
        frontend.fail(OperationId(99), ViewGeneration(1), AppError::from("stale"));
        assert!(frontend.active.is_some());
        frontend.fail(
            OperationId(1),
            ViewGeneration(1),
            AppError::from("page unavailable"),
        );
        assert!(Arc::ptr_eq(&rows, &frontend.state.view.rows));
        assert!(!frontend.state.busy);
        assert!(frontend.active.is_none());
        frontend.submit(Command::Sort, None).unwrap();
        assert!(frontend.active.is_some());
    }

    #[test]
    fn worker_reports_committed_command_refresh_failure_and_recovers() {
        let controller = controller();
        let (send, requests) = mpsc::sync_channel(1);
        let (responses, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || serve(controller, requests, responses));
        assert!(matches!(receive(&receiver), DatabaseResponse::Ready(_)));
        send.send(operation(
            1,
            Command::SqlQuery,
            Some(vec!["DROP TABLE t".into()]),
            Arc::new(AtomicBool::new(false)),
        ))
        .unwrap();
        let DatabaseResponse::OperationFailed { id, error, .. } = receive(&receiver) else {
            panic!("missing refresh failure")
        };
        assert_eq!(id, OperationId(1));
        assert!(error
            .to_string()
            .contains("Command completed; page could not be loaded"));
        send.send(operation(2, Command::SqlQuery, Some(vec!["CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES(1,'recovered')".into()]), Arc::new(AtomicBool::new(false)))).unwrap();
        let DatabaseResponse::Completed { state, error, .. } = receive(&receiver) else {
            panic!("missing recovery")
        };
        assert!(error.is_none());
        assert_eq!(state.view.rows[0][1], DataItem::Text("recovered".into()));
        drop(send);
        worker.join().unwrap().unwrap();
    }
}
