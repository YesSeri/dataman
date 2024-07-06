use std::{
    fmt::{Debug, Display},
    io::{Read, Stdout, Write},
    iter::Sum,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{self, enable_raw_mode},
};
use log::info;
use ratatui::{
    prelude::{Backend, Constraint, CrosstermBackend, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};

use crate::input::InputMode;
use crate::model::datarow::DataTable;
use crate::{
    app_error_other,
    controller::{Command, CommandWrapper},
};
use crate::{
    controller::Controller,
    error::{AppError, AppResult},
    model::database::Database,
};

pub struct TUI {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    is_user_input_active: bool,
}

impl std::fmt::Debug for TUI {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TUI")
            .field("terminal", &self.terminal)
            .finish()
    }
}

impl TUI {
    pub fn new() -> Self {
        let stdout = std::io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();
        enable_raw_mode().unwrap();
        Self {
            terminal,
            is_user_input_active: false,
        }
    }
    pub fn get_table_height() -> AppResult<u16> {
        let height = crossterm::terminal::size()?.1 - 4;
        Ok(height)
    }

    pub fn shutdown(&mut self) -> Result<(), AppError> {
        execute!(
            self.terminal.backend_mut(),
            terminal::Clear(terminal::ClearType::All),
        )?;
        print!("\x1B[2J\x1B[1;1H");
        terminal::disable_raw_mode()?;
        Ok(())
    }
    pub fn draw(controller: &mut Controller) -> AppResult<()> {
        let table_height = TUI::get_table_height().unwrap();
        controller.ui.terminal.draw(|f| {
            match TUI::update(
                f,
                &mut controller.database,
                &controller.last_command,
                table_height,
                controller.input_mode_state_machine.get_state(),
            ) {
                Ok(_) => (),
                Err(err) => {
                    log::error!("Error updating TUI: {:?}", err);
                }
            }
        })?;
        Ok(())
    }

    pub fn get_input() -> AppResult<Command> {
        if let Event::Key(key) = event::read()? {
            let command = Command::from(key);
            Ok(command)
        } else {
            Err(app_error_other!("Could not get input."))
        }
    }

    pub fn get_user_input(data: &str) -> AppResult<String> {
        // let editor_var = std::env::var("EDITOR").unwrap();
        // let editor_and_args = editor_var.split_whitespace().collect::<Vec<_>>();
        // let (editor, args) = editor_and_args.split_first().unwrap_or((&"nano", &[]));
        // let mut args = args.to_vec();
        // let mut file_path = std::env::temp_dir();
        // file_path.push("dataman_input.txt");
        // args.push(file_path.to_str().unwrap());

        // let mut file = std::fs::File::create(&file_path)?;
        // file.write_all(data.as_bytes())?;

        // info!("editor: {:?} args: {:?}", editor, args);
        // std::process::Command::new(editor).args(args).status()?;

        // let mut editable = String::new();
        // std::fs::File::open(file_path)?.read_to_string(&mut editable)?;
        // let trimmed = editable.trim_end_matches('\n').to_string();
        // info!("editable: {:?} | trimmed: {:?}", editable, trimmed);
        // Ok(trimmed)
        todo!()
    }
    fn update(
        f: &mut Frame,
        database: &mut Database,
        last_command: &CommandWrapper,
        table_height: u16,
        input_mode: InputMode,
    ) -> AppResult<()> {
        let constraints: Vec<Constraint> = match input_mode {
            InputMode::Editing => {
                vec![
                    Constraint::Max(1000),
                    Constraint::Length(1),
                    Constraint::Length(5),
                ]
            }
            InputMode::Normal | InputMode::Abort | InputMode::Finish => {
                vec![Constraint::Max(1000), Constraint::Length(1)]
            }
            InputMode::Abort => todo!(),
            InputMode::Finish => todo!(),
            InputMode::ExternalEditor => todo!(),
        };
        let rects = Layout::default()
            .direction(ratatui::prelude::Direction::Vertical)
            .constraints(constraints.as_slice())
            .split(f.size());

        let table_name = database.get_current_table_name()?;

        let (headers, rows): DataTable =
            database.get(100, database.slices[0].row_offset, table_name)?;
        let id_space: u16 = rows.iter().fold(0, |acc, row| {
            let id = row.first().unwrap().to_string().len() as u16;
            if id > acc {
                id
            } else {
                acc
            }
        });

        let widths: Vec<u16> = database.slices[0].column_widths();
        let sum: u16 = widths.iter().sum();
        let constraints = widths
            .iter()
            .map(|w| Constraint::Min(*w))
            .collect::<Vec<_>>();
        let current_header = database.header_idx;
        // mark current header

        let header = Row::new(headers.iter().enumerate().map(|(i, h)| {
            if current_header == i as u16 {
                Cell::from(Span::styled(
                    h.clone(),
                    Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                ))
            } else {
                Cell::from(h.clone())
            }
        }))
        .height(1);

        // draw border under header
        let tui_rows = rows.iter().map(|data_row| {
            let data_row = data_row
                .iter()
                .map(|item| Cell::from(item.clone()))
                .collect::<Vec<_>>();
            Row::new(data_row).height(1)
        });
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let table_name = database.get_current_table_name()?;
        let t = Table::new(tui_rows, constraints)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(table_name))
            .highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut database.slices[0].table_state);

        let a = database.header_idx;
        let row = database.slices[0].table_state.selected().unwrap_or(0);
        let total_rows = database.count_rows().unwrap_or(0);
        // let rowid = rows.get(b).unwrap().data.get(0);
        let rowid = rows
            .get(row)
            .map(|el| el.first().unwrap().to_string())
            .unwrap_or("xxx".to_owned());
        let offset = database.slices[0].table_state.offset();
        let text = vec![Line::from(vec![Span::raw(format!(
            // "last command: {last_command} current header: {a} selected: {b} offset: {offset} "
            "row: {row}, total rows: {total_rows},  last command: {last_command}, height: {table_height}",
        ))])];
        let paragraph = Paragraph::new(text);

        f.render_widget(paragraph, rects[1]);

        if input_mode == InputMode::Editing {
            let input = Paragraph::new(database.input.as_str())
                // show cursor
                .style(Style::default().fg(Color::Yellow))
                .block(Block::bordered().title("Input"));
            f.set_cursor(
                // Draw the cursor at the current position in the input field.
                // This position is can be controlled via the left and right arrow key
                rects[2].x + database.character_index as u16 + 1,
                // Move one line down, from the border to the input line
                rects[2].y + 1,
            );
            f.render_widget(input, rects[2]);
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
}

impl Default for TUI {
    fn default() -> Self {
        Self::new()
    }
}
