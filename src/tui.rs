use std::{
    fmt::{Debug, Display},
    io::{stdout, Read, Stdout, Write},
    iter::Sum,
    panic,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{self, disable_raw_mode, enable_raw_mode, LeaveAlternateScreen},
    ExecutableCommand,
};
use log::info;
use ratatui::{
    prelude::{Backend, Constraint, CrosstermBackend, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};

use crate::{
    app_error_other,
    controller::{self, controller_impl::Controller},
    model::datarow::DataItem,
};
use crate::{controller::input::InputMode, model::datarow::DataTable};
use crate::{
    error::{AppError, AppResult},
    model::database::Database,
};

pub struct TUI {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TUI {
    pub fn new() -> Self {
        let stdout = std::io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();
        enable_raw_mode().unwrap();
        Self { terminal }
    }
    pub fn get_table_height() -> AppResult<u32> {
        let height = (crossterm::terminal::size()?.1 - 4) as u32;
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
    pub fn draw(
        &mut self,
        controller: &mut controller::controller_impl::Controller,
    ) -> AppResult<()> {
        self.terminal
            .draw(|f| match TUI::update(f, &mut controller.database) {
                Ok(_) => (),
                Err(err) => {
                    log::error!("Error updating TUI: {:?}", err);
                }
            })?;
        Ok(())
    }

    pub fn get_input() -> AppResult<controller::command::Command> {
        if let Event::Key(key) = event::read()? {
            let command = controller::command::Command::from(key);
            Ok(command)
        } else {
            Err(app_error_other!("Could not get input."))
        }
    }

    pub fn get_external_editor_input(data: &str) -> AppResult<String> {
        let res = std::env::var("EDITOR");
        let editor_var = match res {
            Err(err) => return Err(app_error_other!(format!("Could not get editor: {:?}", err))),
            Ok(ev) => ev,
        };
        let editor_and_args = editor_var.split_whitespace().collect::<Vec<_>>();
        let (editor, args) = editor_and_args.split_first().unwrap_or((&"nano", &[]));
        let mut args = args.to_vec();
        let mut file_path = std::env::temp_dir();
        file_path.push("dataman_input.txt");
        args.push(file_path.to_str().unwrap());

        let mut file = std::fs::File::create(&file_path)?;
        file.write_all(data.as_bytes())?;

        std::process::Command::new(editor).args(args).status()?;

        let mut editable = String::new();
        std::fs::File::open(file_path)?.read_to_string(&mut editable)?;
        let trimmed = editable.trim_end_matches('\n').to_string();
        Ok(trimmed)
    }

    fn update(f: &mut Frame, database: &mut Database) -> AppResult<()> {
        let constraints: Vec<Constraint> = match database.input_mode_state_machine.get_state() {
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
            InputMode::ExternalEditor => todo!(),
            InputMode::MetadataTable => todo!(),
        };
        let rects = Layout::default()
            .direction(ratatui::prelude::Direction::Vertical)
            .constraints(constraints.as_slice())
            .split(f.size());

        let table_name = database.get_current_table_name()?;

        let (headers, rows): DataTable =
            database.get(100, database.slice.row_offset, table_name)?;
        let id_space: u16 = rows.iter().fold(0, |acc, row| {
            let id = row.first().unwrap().to_string().len() as u16;
            if id > acc {
                id
            } else {
                acc
            }
        });

        let widths: Vec<u16> = database.slice.column_widths();
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
        let tui_rows = TUI::create_tui_rows(&rows, database);
        // let tui_rows = rows.iter().map(|data_row| {
        //     let data_row = data_row
        //         .iter()
        //         .map(|item| Cell::from(item.clone()))
        //         .collect::<Vec<_>>();
        //     Row::new(data_row).height(1)
        // });
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let table_name = database.get_current_table_name()?;
        let t = Table::new(tui_rows, constraints)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(table_name))
            .highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut database.slice.table_state);

        let a = database.header_idx;
        let row = database.slice.table_state.selected().unwrap_or(0);
        let total_rows = database.count_rows().unwrap_or(0);
        let rowid = rows
            .get(row)
            .map(|el| el.first().unwrap().to_string())
            .unwrap_or("xxx".to_owned());
        let offset = database.slice.table_state.offset();
        let last_command = database.last_command.command.to_string();
        let table_height = rects[0].height;
        let text = vec![Line::from(vec![Span::raw(format!(
            // "last command: {last_command} current header: {a} selected: {b} offset: {offset} "
            "row: {row}, total rows: {total_rows},  last command: {last_command}, height: {table_height}",
        ))])];
        let paragraph = Paragraph::new(text);

        f.render_widget(paragraph, rects[1]);

        if database.input_mode_state_machine.get_state() == InputMode::Editing {
            let title = database.last_command.command.to_string();
            let paragraph = Paragraph::new(database.input.as_str())
                .style(Style::default().fg(Color::Yellow))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("{title} input")),
                );

            f.set_cursor(
                rects[2].x + database.character_index as u16 + 1,
                rects[2].y + 1,
            );
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
    fn create_tui_rows(rows: &Vec<Vec<DataItem>>, database: &Database) -> Vec<Row> {
        let tui_rows = rows
            .iter()
            .map(|data_row| {
                let data_row = data_row
                    .iter()
                    .map(|item| Cell::from(item.clone()))
                    .collect::<Vec<_>>();
                Row::new(data_row).height(1)
            })
            .collect();
        if database.input_mode_state_machine.get_state() == InputMode::MetadataTable {
            let mut tui_rows = tui_rows;
            let mut metadata_rows = vec![];
        }
        return tui_rows;
    }

    pub fn install_panic_hook() {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            stdout().execute(LeaveAlternateScreen).unwrap();
            disable_raw_mode().unwrap();
            original_hook(panic_info);
        }));
    }
}

impl Default for TUI {
    fn default() -> Self {
        Self::new()
    }
}
