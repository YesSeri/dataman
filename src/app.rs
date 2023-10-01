use crate::controller::Controller;
use crate::libstuff::db::Database;
use crate::view::{BasicUI, Display};
use ratatui::widgets::TableState;
use rusqlite::Connection;
use std::ops::Add;
use std::path::PathBuf;

#[derive(Debug)]
pub struct App {
    pub state: TableState,
    pub controller: Controller<BasicUI>,
}

impl From<Database> for App {
    fn from(database: Database) -> Self {
        // TODO do a for loop for all sheets
        let controller = Controller::new(BasicUI::new(), database);
        let mut state = TableState::default();
        state.select(Some(0));
        App { state, controller }
    }
}

impl App {
    pub fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => i.add(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }
    pub fn right(&mut self) {
        self.controller.database.next_header();
    }

    pub fn left(&mut self) {
        self.controller.database.previous_header();
    }
}
