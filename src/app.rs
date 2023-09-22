use std::path::PathBuf;
use ratatui::widgets::TableState;
use rusqlite::Connection;
use crate::libstuff::db::Database;
use crate::model::Sheet;

pub struct App {
    pub state: TableState,
    pub sheet: Sheet,
    pub name: String,
}
impl From<Database> for App {
    fn from(database: Database) -> Self {
        let name = database.table_names[0].clone();
        let sheet = Sheet::try_from(database).unwrap();
        let mut state = TableState::default();
        state.select(Some(0));
        App {
            state, sheet, name,
        }

    }
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
