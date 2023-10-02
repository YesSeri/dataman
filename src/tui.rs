use std::{io::Stdout, thread, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::{Backend, Constraint, CrosstermBackend, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    symbols::block,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table},
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
                let term_height = controller.ui.terminal.backend().size()?.height;
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') => controller.copy(),
                        KeyCode::Right => controller.database.next_header(),
                        KeyCode::Left => controller.database.previous_header(),
                        KeyCode::Down => controller.database.next_row(term_height),
                        KeyCode::Up => controller.database.previous_row(term_height),
                        _ => {}
                    }
                }
            }
        }
    }
    fn update<B: Backend>(f: &mut Frame<B>, db: &mut Database) {
        let rects = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Max(1000), Constraint::Length(1)].as_ref())
            .split(f.size());

        let table_name = db.table_names.iter().next().unwrap();
        let (headers, rows) = db.get(150, table_name);
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
        let rows = rows.iter().map(|items| {
            let height = 1;
            Row::new(items.clone()).height(height as u16)
        });
        let selected_style = Style::default().add_modifier(Modifier::UNDERLINED);
        let t = Table::new(rows)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("MY TITLE"))
            .highlight_style(selected_style)
            // .highlight_symbol(">> ")
            .widths(widths.as_slice())
            .bg(Color::Black);
        f.render_stateful_widget(t, rects[0], &mut db.state);

        let a = db.current_header_idx;
        let b = db.state.selected().unwrap_or(200);
        let text = vec![Line::from(vec![
            Span::raw(format!("current header: {}", a)),
            Span::raw(format!("selected: {}", b)),
        ])];
        let paragraph = Paragraph::new(text);

        f.render_widget(paragraph, rects[1])
    }
}

impl Default for TUI {
    fn default() -> Self {
        Self::new()
    }
}
