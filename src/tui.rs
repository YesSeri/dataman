use core::panic;
use std::{
    fmt::Display,
    io::{Read, Stdout, Write},
    process::exit,
    result, thread,
    time::Duration,
};

use crossterm::event::KeyModifiers;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::{Backend, Constraint, CrosstermBackend, Layout},
    style::{Color, Modifier, Style, Stylize},
    symbols::block,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table},
    Frame, Terminal,
};

use crate::error::log;
use crate::{
    controller::{Command, CommandWrapper, Controller, Direction},
    error::{AppError, AppResult},
    libstuff::db::Database,
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
    pub fn get_terminal_height(&self) -> AppResult<u16> {
        let size = self.terminal.backend().size()?;
        Ok(size.height)
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
    pub fn start(controller: &mut Controller) -> Result<Command, AppError> {
        controller.ui.terminal.draw(|f| {
            match TUI::update(f, &mut controller.database, &controller.last_command) {
                Ok(_) => (),
                Err(_) => {
                    terminal::disable_raw_mode().unwrap();
                    panic!("error in update");
                }
            }
        })?;
        if let Event::Key(key) = event::read()? {
            let term_height = controller.ui.terminal.backend().size()?.height;
            let command = Command::from(key);
            Ok(command)
        } else {
            Err(AppError::Other)
        }
    }

    pub fn get_editor_input(data: &str) -> AppResult<String> {
        let editor = std::env::var("EDITOR").unwrap();
        let mut file_path = std::env::temp_dir();
        file_path.push("dataman_input.txt");

        let mut file = std::fs::File::create(&file_path)?;
        file.write_all(data.as_bytes())?;

        std::process::Command::new(editor)
            .arg(&file_path)
            .status()?;

        let mut editable = String::new();
        std::fs::File::open(file_path)?.read_to_string(&mut editable)?;
        let trimmed = editable.trim_end_matches('\n').to_string();
        Ok(trimmed)
    }
    fn update<B: Backend>(
        f: &mut Frame<B>,
        database: &mut Database,
        last_command: &CommandWrapper,
    ) -> AppResult<()> {
        let rects = Layout::default()
            .direction(ratatui::prelude::Direction::Vertical)
            .constraints([Constraint::Max(1000), Constraint::Length(1)].as_ref())
            .split(f.size());

        let table_name = database.get_current_table_name()?;

        let (headers, rows) = database.get(100, 0, table_name)?;

        let id_extra_space = 8 / headers.len() as u16;

        let per_header = (100 / headers.len()) as u16 - id_extra_space;
        let widths = headers
            .iter()
            .map(|header| {
                if header == "id" {
                    Constraint::Length(8)
                } else {
                    Constraint::Percentage(per_header)
                }
            })
            .collect::<Vec<_>>();
        let current_header: usize = database.header_idx;
        // mark current header

        let header = Row::new(headers.iter().enumerate().map(|(i, h)| {
            if current_header == i {
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
        let tui_rows = rows.iter().map(|data_row| data_row.to_tui_row().height(1));
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let table_name = database.get_current_table_name()?;
        let t = Table::new(tui_rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(table_name))
            .highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .widths(widths.as_slice())
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut database.state);

        let a = database.header_idx;
        let b = database.state.selected().unwrap_or(0);
        // let rowid = rows.get(b).unwrap().data.get(0);
        let row = rows.get(b).unwrap();
        let item = row.get(0);
        let rowid = item.to_string();
        let c: String = database
            .count_headers()
            .map(|r| r.to_string())
            .unwrap_or("???".to_string());
        let offset = database.state.offset();
        let text = vec![Line::from(vec![Span::raw(format!(
            // "last command: {last_command} current header: {a} selected: {b} offset: {offset} "
            "rowid {rowid}, last command: {last_command}",
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
