#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

mod model;
pub mod view;
pub mod controller;
use model::*;

use std::{error::Error, fs::File};

use csv::ReaderBuilder;

#[derive(Debug)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug)]
pub struct Editor {
    sheet: Sheet,
}

impl Editor {
    fn new(sheet: Sheet) -> Self {
        Self { sheet }
    }
}

enum Command {
    MoveCursor(Direction),
    EditColumn(String),
    Save,
}

#[derive(Debug)]
pub struct UIComponent {}
impl UIComponent {
    fn new() -> Self {
        Self {}
    }
}

#[derive(Debug)]
pub struct App {
    pub editor: Editor,
    pub ui: UIComponent,
}

impl App {
    pub fn new() -> Self {
        let columns = vec![];
        let sheet = Sheet::new(columns, 0);
        Self {
            editor: Editor::new(sheet),
            ui: UIComponent::new(),
        }
    }
    pub fn derive(&mut self, i: usize, fun: fn(String) -> String) {
        let column = &self.editor.sheet.columns[i];
        let new_column = Column::derive_new(column, fun);
        self.editor.sheet.columns.push(new_column);
    }
    pub fn save(&self, file_path: &str) -> Result<(), Box<dyn Error>> {
        let mut wtr = csv::Writer::from_path(file_path)?;
        for column in self.editor.sheet.columns.iter() {
            wtr.write_record(column.get_data())?;
        }
        wtr.flush()?;
        Ok(())
    }

}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn derive() {
       // let file_path = "assets/data.csv";
       // assert_eq!(app.editor.sheet.columns.len(), 3);
       // app.derive(1, |cell| format!("{}X{}", cell, cell));

       // let text = app.editor.sheet.get(1, 1);
       // assert_eq!("zenkert".to_string(), text);

       // let text = app.editor.sheet.get(3, 1);
       // assert_eq!("zenkertXzenkert".to_string(), text);
       // assert_eq!(app.editor.sheet.columns.len(), 4);
    }
}
