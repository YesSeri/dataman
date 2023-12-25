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
use ratatui::{
    prelude::{Backend, Constraint, CrosstermBackend, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};

use crate::model::datarow::DataTable;
use crate::{
    controller::Controller,
    error::{AppError, AppResult},
    model::database::Database,
};
use crate::{
    controller::{Command, CommandWrapper},
    error::log,
};

pub struct TUI {
    terminal: Terminal<CrosstermBackend<Stdout>>,
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
        Self { terminal }
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
            ) {
                Ok(_) => (),
                Err(err) => {
                    terminal::disable_raw_mode().unwrap();
                    panic!("error in update");
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
            Err(AppError::Other)
        }
    }

    pub fn get_editor_input(data: &str) -> AppResult<String> {
        let editor_var = std::env::var("EDITOR").unwrap();
        let editor_and_args = editor_var.split_whitespace().collect::<Vec<_>>();
        let (editor, args) = editor_and_args.split_first().unwrap_or((&"nano", &[]));
        let mut args = args.to_vec();
        let mut file_path = std::env::temp_dir();
        file_path.push("dataman_input.txt");
        args.push(file_path.to_str().unwrap());

        let mut file = std::fs::File::create(&file_path)?;
        file.write_all(data.as_bytes())?;

        log(format!("editor: {:?} args: {:?}", editor, args));
        std::process::Command::new(editor).args(args).status()?;

        let mut editable = String::new();
        std::fs::File::open(file_path)?.read_to_string(&mut editable)?;
        let trimmed = editable.trim_end_matches('\n').to_string();
        log(format!("editable: {:?} | trimmed: {:?}", editable, trimmed));
        Ok(trimmed)
    }
    fn update<B: Backend>(
        f: &mut Frame<B>,
        database: &mut Database,
        last_command: &CommandWrapper,
        table_height: u16,
    ) -> AppResult<()> {
        let rects = Layout::default()
            .direction(ratatui::prelude::Direction::Vertical)
            .constraints([Constraint::Max(1000), Constraint::Length(1)].as_ref())
            .split(f.size());

        let table_name = database.get_current_table_name()?;

        let (headers, rows): DataTable =
            database.get(100, database.slices[0].row_offset, table_name)?;
        let id_space: u16 = rows.iter().fold(0, |acc, row| {
            let id = row.get(0).unwrap().to_string().len() as u16;
            if id > acc {
                id
            } else {
                acc
            }
        });

        let per_header = (100 - id_space) / (headers.len() - 1) as u16;
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

        // // draw border under header
        let tui_rows = rows.iter().map(|data_row| {
            let data_row = data_row
                .iter()
                .map(|item| Cell::from(item.clone()))
                .collect::<Vec<_>>();
            Row::new(data_row).height(1)
        });
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let table_name = database.get_current_table_name()?;
        let t = Table::new(tui_rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(table_name))
            .highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .widths(constraints.as_slice())
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut database.slices[0].table_state);

        let a = database.header_idx;
        let row = database.slices[0].table_state.selected().unwrap_or(0);
        let total_rows = database.count_rows().unwrap_or(0);
        // let rowid = rows.get(b).unwrap().data.get(0);
        let rowid = rows
            .get(row)
            .map(|el| el.get(0).unwrap().to_string())
            .unwrap_or("xxx".to_owned());
        let offset = database.slices[0].table_state.offset();
        let text = vec![Line::from(vec![Span::raw(format!(
            // "last command: {last_command} current header: {a} selected: {b} offset: {offset} "
            "row: {row}, total rows: {total_rows},  last command: {last_command}, height: {table_height}",
        ))])];
        let paragraph = Paragraph::new(text);

        f.render_widget(paragraph, rects[1]);
        Ok(())
    }
}

impl Default for TUI {
    fn default() -> Self {
        Self::new()
    }
}
