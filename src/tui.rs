use std::{io::Stdout, thread, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::CrosstermBackend,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
    Terminal,
};

use crate::{error::AppError, view::Display};

pub struct TUI {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}
impl Display for TUI {
    fn update(&mut self, headers: Vec<String>, rows: Vec<Vec<String>>) -> Result<(), AppError> {
        execute!(std::io::stdout(), terminal::Clear(terminal::ClearType::All))?;
        // self.terminal.draw(|f| {
        //     let size = f.size();
        //     let block = Block::default().title("Block").borders(Borders::ALL);
        //     f.render_widget(block, size);
        // })?;

        // Start a thread to discard any input events. Without handling events, the
        // stdin buffer will fill up, and be read into the shell when the program exits.

        loop {
            let items = [
                ListItem::new("Item 1"),
                ListItem::new("Item 2"),
                ListItem::new("Item 3"),
            ];
            self.terminal.draw(|f| {
                let size = f.size();
                // show some text inside block
                let list = List::new(items)
                    .block(Block::default().title("List").borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
                    .highlight_symbol(">>");

                let title = Span::styled(
                    "This is a line of colored text",
                    Style::default().fg(Color::Red),
                );
                let block = Block::default().title(title).borders(Borders::ALL);
                f.render_widget(list, size);
            })?;
            if should_quit()? {
                break;
            }
        }
        Ok(())
    }

    fn new() -> Self {
        enable_raw_mode().unwrap();
        let stdout = std::io::stdout();
        execute!(&stdout, EnterAlternateScreen).unwrap();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();
        Self { terminal }
    }

    fn shutdown(&mut self) -> Result<(), AppError> {
        execute!(
            self.terminal.backend_mut(),
            terminal::Clear(terminal::ClearType::All),
            LeaveAlternateScreen
        )?;
        terminal::disable_raw_mode()?;
        Ok(())
    }
}

fn should_quit() -> Result<bool, AppError> {
    if event::poll(Duration::from_millis(250))? {
        if let Event::Key(key) = event::read()? {
            return Ok(KeyCode::Char('q') == key.code);
        }
    }
    Ok(false)
}
