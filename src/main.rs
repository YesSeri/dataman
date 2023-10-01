use std::path::Path;
use std::process::id;
use std::{error::Error, io, path::PathBuf};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dataman::app::App;
use dataman::libstuff::db::Database;
use ratatui::text::Spans;
use ratatui::{prelude::*, widgets::*};

fn main() -> Result<(), Box<dyn Error>> {
    let p = Path::new("assets/data.csv");

    let database = Database::try_from(p).unwrap();
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let app = App::from(database);
    let res = run_app(&mut terminal, app);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &mut app))?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    // KeyCode::Char('r') => app.regex(),
                    KeyCode::Right => app.right(),
                    KeyCode::Left => app.left(),
                    KeyCode::Down => app.next(),
                    KeyCode::Up => app.previous(),
                    _ => {}
                }
            }
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app: &mut App) {
    let rects = Layout::default()
        .constraints([Constraint::Percentage(100)].as_ref())
        .split(f.size());

    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let (headers, rows) = app.controller.get_headers_and_rows(20).unwrap();
    let per_header = (100 / 4) as u16;
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
    let current_header: u32 = app.controller.database.current_header;
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
    f.render_stateful_widget(t, rects[0], &mut app.state);
}
