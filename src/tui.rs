use std::{
    io::{stdout, Stdout, Write},
    panic,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{self, disable_raw_mode, enable_raw_mode, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::{Constraint, CrosstermBackend, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};

use crate::controller;

fn data_item_cell(item: &DataItem) -> Cell<'_> {
    match item {
        DataItem::Text(value) => Cell::from(value.as_str()),
        DataItem::Integer(value) => Cell::from(value.to_string()),
        DataItem::Float(value) => Cell::from(value.to_string()),
        DataItem::Blob(value) => Cell::from(
            value
                .iter()
                .take(40)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
        DataItem::Null => Cell::from("NULL"),
    }
}

pub(crate) fn column_widths(
    headers: &[String],
    rows: &[Vec<DataItem>],
    indexes: &[usize],
) -> Vec<u16> {
    let mut widths: Vec<u16> = headers.iter().map(|header| header.len() as u16).collect();
    for row in rows {
        for (visible, source) in indexes.iter().enumerate() {
            if let (Some(width), Some(item)) = (widths.get_mut(visible), row.get(*source)) {
                *width = (*width).max(item.len().min(u16::MAX as usize) as u16);
            }
        }
    }
    // Cap individual cells in one pass; horizontal scrolling handles wide tables.
    for width in &mut widths {
        *width = (*width).clamp(1, 80);
    }
    widths
}

fn table_height_for(terminal_height: u16) -> u32 {
    u32::from(terminal_height.saturating_sub(4))
}

fn column_window(widths: &[u16], selected: usize, available: u16) -> std::ops::Range<usize> {
    if widths.is_empty() {
        return 0..0;
    }
    let selected = selected.min(widths.len() - 1);
    let mut start = selected;
    let mut end = selected + 1;
    let mut used = widths[selected].max(1);
    while end < widths.len() && used.saturating_add(widths[end].max(1)) <= available {
        used += widths[end].max(1);
        end += 1;
    }
    while start > 0 && used.saturating_add(widths[start - 1].max(1)) <= available {
        start -= 1;
        used += widths[start].max(1);
    }
    start..end
}

fn trim_external_editor_newline(editable: String) -> String {
    if let Some(content) = editable.strip_suffix("\r\n") {
        content.to_owned()
    } else if let Some(content) = editable.strip_suffix('\n') {
        content.to_owned()
    } else {
        editable
    }
}
use crate::error::{AppError, AppResult};
use crate::{controller::input::InputMode, model::datarow::DataItem};

pub struct TUI {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    restored: bool,
}

impl TUI {
    pub fn new() -> AppResult<Self> {
        let result = (|| -> AppResult<Self> {
            let backend = CrosstermBackend::new(std::io::stdout());
            let terminal = Terminal::new(backend)?;
            enable_raw_mode()?;
            Ok(Self {
                terminal,
                restored: false,
            })
        })();
        match result {
            Ok(tui) => Ok(tui),
            Err(error) => crate::error::finish(
                Err(error.context("Initializing terminal")),
                restore_terminal(),
            ),
        }
    }
    pub fn get_table_height() -> AppResult<u32> {
        Ok(table_height_for(crossterm::terminal::size()?.1))
    }

    pub fn shutdown(&mut self) -> AppResult<()> {
        if self.restored {
            return Ok(());
        }
        let result = restore_terminal();
        self.restored = result.is_ok();
        result
    }
    pub(crate) fn draw(
        &mut self,
        controller: &crate::controller::worker::RenderState,
    ) -> AppResult<()> {
        let mut update = Ok(());
        self.terminal
            .draw(|frame| update = Self::update(frame, controller))?;
        update
    }

    pub fn get_input() -> AppResult<controller::command::Command> {
        if let Event::Key(key) = event::read()? {
            let command = controller::command::Command::from(key);
            Ok(command)
        } else {
            Err(AppError::invalid_input("Could not get input."))
        }
    }

    pub fn get_external_editor_input(data: &str) -> AppResult<String> {
        let res = std::env::var("EDITOR");
        let editor_var = match res {
            Err(err) => {
                return Err(AppError::invalid_input(format!(
                    "Could not get editor: {:?}",
                    err
                )))
            }
            Ok(ev) => ev,
        };
        let editor_and_args = editor_var.split_whitespace().collect::<Vec<_>>();
        let (editor, args) = editor_and_args.split_first().unwrap_or((&"nano", &[]));
        let mut args = args.to_vec();
        let mut temporary = tempfile::Builder::new()
            .prefix("dataman-editor-")
            .suffix(".txt")
            .tempfile()?;
        temporary.write_all(data.as_bytes())?;
        temporary.flush()?;
        args.push(
            temporary
                .path()
                .to_str()
                .ok_or_else(|| AppError::from("Invalid editor path"))?,
        );
        let status = std::process::Command::new(editor).args(args).status()?;
        if !status.success() {
            return Err(AppError::Message(format!("Editor exited with {status}")));
        }
        let editable = std::fs::read_to_string(temporary.path())?;
        let trimmed = trim_external_editor_newline(editable);
        Ok(trimmed)
    }

    fn update(f: &mut Frame, controller: &crate::controller::worker::RenderState) -> AppResult<()> {
        let (ui, view) = (&controller.ui, &controller.view);
        let constraints: Vec<Constraint> = match ui.input_mode_state_machine.get_state() {
            InputMode::Editing => {
                vec![
                    Constraint::Max(1000),
                    Constraint::Length(1),
                    Constraint::Length(3),
                ]
            }
            InputMode::Normal | InputMode::Abort | InputMode::Finish => {
                vec![Constraint::Max(1000), Constraint::Length(1)]
            }
            InputMode::ExternalEditor => {
                return Err(AppError::from("External editor is still active"))
            }
        };
        let rects = Layout::default()
            .direction(ratatui::prelude::Direction::Vertical)
            .constraints(constraints.as_slice())
            .split(f.area());

        if ui.history_view {
            let block = Block::default().borders(Borders::ALL).title(format!(
                "Command History (↑/↓ select, Enter replay, Esc return) {}",
                ui.last_command.message.as_deref().unwrap_or("")
            ));
            let first = ui
                .history_cursor
                .saturating_sub(29)
                .min(controller.command_history.len().saturating_sub(30));
            let lines = (first..controller.command_history.len())
                .map(|index| {
                    let entry = &controller.command_history[index];
                    let marker = if index == ui.history_cursor {
                        "> "
                    } else {
                        "  "
                    };
                    Line::from(format!(
                        "{marker}{}  {}  {}",
                        entry.command,
                        entry.table,
                        entry.inputs.join(" | ")
                    ))
                })
                .collect::<Vec<_>>();
            f.render_widget(Paragraph::new(lines).block(block), f.area());
            return Ok(());
        }
        let headers = &view.headers;
        let rows = &view.rows;
        let widths = &controller.column_widths;
        let window = column_window(widths, view.column, f.area().width.saturating_sub(4));
        let constraints = widths[window.clone()]
            .iter()
            .map(|w| Constraint::Min(*w))
            .collect::<Vec<_>>();
        let current_header = view.column;
        // mark current header

        let header = Row::new(headers[window.clone()].iter().enumerate().map(|(i, h)| {
            let label = if controller.marked_join_keys.iter().any(|key| key == h) {
                format!("!{h}")
            } else {
                h.clone()
            };
            if current_header == window.start + i {
                Cell::from(Span::styled(
                    label,
                    Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                ))
            } else if controller.marked_join_keys.iter().any(|key| key == h) {
                Cell::from(Span::styled(label, Style::default().fg(Color::Yellow)))
            } else {
                Cell::from(label)
            }
        }))
        .height(1);

        // draw border under header
        let selected_ids = &controller.selected_rows;
        let tui_rows = rows.iter().map(|data_row| {
            let row_id = data_row
                .get(view.column_indexes.first().copied().unwrap_or(0))
                .and_then(|item| item.to_string().parse::<i64>().ok());
            let selected_sheet = if view.table_name == "table_of_tables" {
                let name_column = view
                    .headers
                    .iter()
                    .position(|header| header == "table_name")
                    .and_then(|position| view.column_indexes.get(position).copied());
                name_column
                    .and_then(|index| data_row.get(index))
                    .is_some_and(|name| controller.selected_sheets.contains(&name.to_string()))
            } else {
                false
            };
            let selected_style =
                if selected_sheet || row_id.is_some_and(|id| selected_ids.contains(&id)) {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
            let data_row = view.column_indexes[window.clone()]
                .iter()
                .filter_map(|index| data_row.get(*index))
                .map(data_item_cell)
                .collect::<Vec<_>>();
            Row::new(data_row).height(1).style(selected_style)
        });
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let t = Table::new(tui_rows, constraints)
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(view.table_name.as_str()),
            )
            .row_highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .bg(Color::Black);
        let row = view.row;
        let mut table_state = TableState::default();
        table_state.select(Some(row));
        f.render_stateful_widget(t, rects[0], &mut table_state);

        let total_rows = controller.total_rows;
        let last_command = ui.last_command.command.to_string();
        let status = ui.last_command.message.as_deref().unwrap_or("");
        let busy = if controller.busy {
            format!(
                " {} {}… Esc cancels",
                ["◐", "◓", "◑", "◒"][controller.busy_frame % 4],
                controller.busy_phase
            )
        } else {
            String::new()
        };
        let table_height = rects[0].height;
        let selected_count = if view.table_name == "table_of_tables" {
            controller.selected_sheets.len()
        } else {
            controller.selected_rows.len()
        };
        let text = vec![Line::from(vec![Span::raw(format!(
            // "last command: {last_command} current header: {a} selected: {b} offset: {offset} "
            "row: {row}, total rows: {total_rows}, selected: {}, last command: {last_command} {status}{busy}, height: {table_height}",
            selected_count,
        ))])];
        let paragraph = Paragraph::new(text);

        f.render_widget(paragraph, rects[1]);

        if ui.input_mode_state_machine.get_state() == InputMode::Editing {
            let title = ui.last_command.command.to_string();
            let paragraph = Paragraph::new(ui.input.as_str())
                .style(Style::default().fg(Color::Yellow))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("{title} input")),
                );

            f.set_cursor_position((rects[2].x + ui.character_index as u16 + 1, rects[2].y + 1));
            f.render_widget(paragraph, rects[2]);
            // match app.input_mode {
            //     InputMode::Normal =>
            //         // Hide the cursor. `Frame` does this by default, so we don't need to do anything here
            //         {}

            //     InputMode::Editing => {
            //         // Make the cursor visible and ask ratatui to put it at the specified coordinates after
            //         // rendering
            //         #[allow(clippy::cast_possible_truncation)]
            //         f.set_cursor(
            //             // Draw the cursor at the current position in the input field.
            //             // This position is can be controlled via the left and right arrow key
            //             input_area.x + app.character_index as u16 + 1,
            //             // Move one line down, from the border to the input line
            //             input_area.y + 1,
            //         );
            //     }
            // }
        }
        Ok(())
    }

    pub fn install_panic_hook() {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            let _ = restore_terminal();
            original_hook(panic_info);
        }));
    }
}

/// Each step is attempted even if an earlier step fails.
fn restore_steps(steps: &mut [&mut dyn FnMut() -> AppResult<()>]) -> AppResult<()> {
    let mut result = Ok(());
    for step in steps {
        result = crate::error::finish(result, step());
    }
    result
}

fn restore_terminal() -> AppResult<()> {
    restore_steps(&mut [
        &mut || {
            execute!(stdout(), LeaveAlternateScreen)?;
            Ok(())
        },
        &mut || {
            execute!(stdout(), crossterm::cursor::Show)?;
            Ok(())
        },
        &mut || {
            disable_raw_mode()?;
            Ok(())
        },
    ])
}

impl Drop for TUI {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::{column_window, table_height_for, trim_external_editor_newline};
    use crate::model::database::Database;

    #[test]
    #[ignore = "manual rendering benchmark"]
    fn benchmark_cached_render() {
        use crate::controller::{controller_impl::Controller, worker::RenderState};
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT, amount INTEGER);
            WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<100) INSERT INTO t SELECT id,'some text',id FROM n;").unwrap();
        let mut controller = Controller::new(Database::new(connection).unwrap()).unwrap();
        controller.get_headers_and_rows(100).unwrap();
        let state = RenderState::from_controller(&controller).unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            terminal
                .draw(|frame| super::TUI::update(frame, &state).unwrap())
                .unwrap();
        }
        eprintln!(
            "cached_render_us={:.2}",
            start.elapsed().as_secs_f64() * 1_000_000.0 / 10_000.0
        );
    }

    #[test]
    fn cached_render_handles_resize_typing_and_history_without_database_access() {
        use crate::controller::{controller_impl::Controller, worker::RenderState};
        use rusqlite::{
            hooks::{AuthContext, Authorization},
            Connection,
        };
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO t VALUES(1,'hello');").unwrap();
        let mut controller = Controller::new(Database::new(connection).unwrap()).unwrap();
        controller.get_headers_and_rows(100).unwrap();
        let mut state = RenderState::from_controller(&controller).unwrap();
        controller
            .database
            .connection
            .authorizer(Some(|_: AuthContext<'_>| Authorization::Deny));
        for (width, height) in [(80, 24), (20, 8), (160, 40)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::TUI::update(frame, &state).unwrap())
                .unwrap();
            state.ui.input.push('x');
            state.ui.history_view = !state.ui.history_view;
            terminal
                .draw(|frame| super::TUI::update(frame, &state).unwrap())
                .unwrap();
        }
    }

    #[test]
    fn short_terminal_has_zero_table_height() {
        assert_eq!(table_height_for(3), 0);
    }

    #[test]
    fn external_editor_removes_only_the_final_newline() {
        assert_eq!(trim_external_editor_newline("first\n\n".into()), "first\n");
    }

    #[test]
    fn selected_column_is_kept_inside_horizontal_window() {
        assert_eq!(column_window(&[10, 10, 10, 10], 3, 20), 2..4);
        assert_eq!(column_window(&[10, 10, 10, 10], 0, 20), 0..2);
    }
    #[test]
    fn restoration_attempts_every_step_and_preserves_failures() {
        use std::cell::Cell;
        let attempts = Cell::new(0);
        let error = super::restore_steps(&mut [
            &mut || {
                attempts.set(attempts.get() + 1);
                Err(crate::error::AppError::from("screen"))
            },
            &mut || {
                attempts.set(attempts.get() + 1);
                Ok(())
            },
            &mut || {
                attempts.set(attempts.get() + 1);
                Err(crate::error::AppError::from("raw mode"))
            },
        ])
        .unwrap_err();
        assert_eq!(attempts.get(), 3);
        assert!(error.to_string().contains("screen"));
        assert!(error.to_string().contains("raw mode"));
    }
}
