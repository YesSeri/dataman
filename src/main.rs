use std::{error::Error, io, path::PathBuf};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dataman::model::Sheet;
use ratatui::{prelude::*, widgets::*};
use dataman::db;

struct App {
    state: TableState,
    sheet: Sheet,
    name: String,
}

impl<'a> App {
    fn from(path: PathBuf) -> App {
        let name = (&path).display().to_string();
        let sheet = Sheet::try_from(path).unwrap();
        let mut state = TableState::default();
        state.select(Some(0));
        App {
            state, sheet, name,
        }
    }
    pub fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.sheet.columns.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 1,
        };
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.sheet.columns.len() - 2
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }
    pub fn right(&mut self){

    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let p = PathBuf::from("assets/data.csv");
    db::open_connection(&p).unwrap();
    todo!();
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let file = "assets/data.csv";
    let app = App::from(PathBuf::from(file));
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
                    KeyCode::Right => app.right(),
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
    let header_cells: Vec<String> = app
        .sheet
        .columns
        .iter()
        .map(|col| col.data.first().unwrap().clone())
        .collect::<Vec<String>>();
    let header = Row::new(header_cells).style(Style::default().bold());
    // draw border under header
    let rows = app.sheet.columns.iter().skip(1).map(|item| {
        let height = 1;
        let cells = item.data.iter().map(|c| Cell::from(c.clone()));
        Row::new(cells).height(height as u16)
    });
    let t = Table::new(rows)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(app.name.clone()))
        .highlight_style(selected_style)
        .highlight_symbol(">> ")
        .widths(&[
            Constraint::Percentage(50),
            Constraint::Max(30),
            Constraint::Min(10),
        ]);
    f.render_stateful_widget(t, rects[0], &mut app.state);
}
