use std::{io::Stdout, thread, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::{Backend, Constraint, CrosstermBackend, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, List, ListItem, Row, Table},
    Frame, Terminal,
};

use crate::{controller::Controller, error::AppError, libstuff::db::Database};

pub struct TUI {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}
impl TUI {
    pub fn new() -> Self {
        let stdout = std::io::stdout();
        execute!(&stdout, EnterAlternateScreen).unwrap();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();
        enable_raw_mode().unwrap();
        Self { terminal }
    }

    pub fn shutdown(&mut self) -> Result<(), AppError> {
        execute!(
            self.terminal.backend_mut(),
            terminal::Clear(terminal::ClearType::All),
            LeaveAlternateScreen
        )?;
        terminal::disable_raw_mode()?;
        Ok(())
    }
    pub fn start(controller: &mut Controller) -> Result<(), AppError> {
        loop {
            controller
                .ui
                .terminal
                .draw(|f| TUI::update(f, &mut controller.database))?;
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') => controller.copy(),
                        KeyCode::Right => controller.database.right(),
                        KeyCode::Left => controller.database.left(),
                        KeyCode::Down => controller.database.next(),
                        KeyCode::Up => controller.database.previous(),
                        _ => {}
                    }
                }
            }
        }
    }
    fn update<B: Backend>(f: &mut Frame<B>, db: &mut Database) {
        let rects = Layout::default()
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(f.size());

        let selected_style = Style::default().add_modifier(Modifier::REVERSED);
        let (headers, rows) = db.get(20, "data");
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
        let current_header: u32 = db.current_header_idx;
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
        let rows = rows.iter().map(|item| {
            let height = 1;
            let cells = item.iter().map(|c| Cell::from(c.clone()));
            Row::new(cells).height(height as u16)
        });
        let t = Table::new(rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("MY TITLE"))
            .highlight_style(selected_style)
            .highlight_symbol(">> ")
            .widths(widths.as_slice())
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut db.state);
    }
}

impl Default for TUI {
    fn default() -> Self {
        Self::new()
    }
}
