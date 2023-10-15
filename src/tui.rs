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
            // if key.kind == KeyEventKind::Press {
            // match key.code {
            //     KeyCode::Char('q') => {
            //         // controller.ui.set_command(Command::SqlQuery(query.clone()));
            //         // controller.sql_query()
            //         Ok(Command::SqlQuery)
            //     }
            //     // KeyCode::Char('f') => controller.regex_filter(),
            //     KeyCode::Char('c') => {
            //         if key.modifiers.contains(KeyModifiers::CONTROL) {
            //             Ok(Command::Quit)
            //         } else {
            //             // controller.copy()
            //             Ok(Command::None)
            //         }
            //     }

            //     KeyCode::Char('r') => Ok(Command::Regex),
            //     KeyCode::Char('e') => Ok(Command::Edit),
            //     KeyCode::Char('s') => {
            //         if key.modifiers.contains(KeyModifiers::CONTROL) {
            //             Ok(Command::Save)
            //         } else {
            //             Ok(Command::Copy)
            //         }
            //     }
            //     // KeyCode::Char('r') => controller.regex(),
            //     // KeyCode::Char('e') => controller.edit_cell(),
            //     // KeyCode::Char('s') => {
            //     //     if key.modifiers.contains(KeyModifiers::CONTROL) {
            //     //         controller.save()
            //     //     } else {
            //     //         controller.sort()
            //     //     }
            //     // }
            //     // KeyCode::Right | KeyCode::Left | KeyCode::Down | KeyCode::Up => {
            //     //     let direction = match key.code {
            //     //         KeyCode::Right => Direction::Right,
            //     //         KeyCode::Left => Direction::Left,
            //     //         KeyCode::Down => Direction::Down,
            //     //         KeyCode::Up => Direction::Up,
            //     //         _ => unreachable!(),
            //     //     };
            //     //     Ok(Command::Move(direction))
            //     // }
            //     KeyCode::Right => Ok(Command::Move(Direction::Right)),
            //     KeyCode::Left => Ok(Command::Move(Direction::Left)),
            //     KeyCode::Down => Ok(Command::Move(Direction::Down)),
            //     KeyCode::Up => Ok(Command::Move(Direction::Up)),

            //     _ => Ok(Command::None),
            // }
            // } else {
            //     Ok(todo!())
            // }
        } else {
            Ok(todo!())
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
        Ok(editable)
    }
    fn update<B: Backend>(
        f: &mut Frame<B>,
        db: &mut Database,
        last_command: &CommandWrapper,
    ) -> AppResult<()> {
        let rects = Layout::default()
            .direction(ratatui::prelude::Direction::Vertical)
            .constraints([Constraint::Max(1000), Constraint::Length(1)].as_ref())
            .split(f.size());

        let binding = "default table name".to_string();
        let table_name = db.get_current_table_name()?;
        let (headers, rows) = db.get(150, 0, table_name)?;
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
        let current_header: u32 = db.header_idx;
        // mark current header

        let header = Row::new(headers.iter().enumerate().map(|(i, h)| {
            if current_header == i as u32 {
                Cell::from(Span::styled(
                    h.clone(),
                    Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                ))
            } else {
                Cell::from(h.clone())
            }
        }))
        // style with bold
        // .style(Style::default().add_modifier(Modifier::BOLD))
        .height(1);

        // // draw border under header
        let rows = rows.iter().map(|items| {
            let height = 1;
            Row::new(items.clone()).height(height as u16)
        });
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let binding = "default table name".to_string();
        let table_name = db.get_current_table_name()?;
        let t = Table::new(rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(table_name))
            .highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .widths(widths.as_slice())
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut db.state);

        let a = db.header_idx;
        let b = db.state.selected().unwrap_or(200);
        let c: String = db
            .count_headers()
            .map(|r| r.to_string())
            .unwrap_or("???".to_string());
        let offset = db.state.offset();
        let text = vec![Line::from(vec![Span::raw(format!(
            // "last command: {last_command} current header: {a} selected: {b} offset: {offset} "
            "last command: {last_command}"
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
