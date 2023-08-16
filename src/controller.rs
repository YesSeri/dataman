use std::path::PathBuf;

use crate::{model::Sheet, view::{Display}};

pub struct Controller<T: Display + Default> {
    ui: T,
    model: Sheet,
}

impl<T: Display + Default> Controller<T> {
    pub fn new(ui: T, model: Sheet) -> Self {
        Self { ui, model } 
    }
    pub fn from(pathbuf: &PathBuf) -> Self {
        Self { ui: Display::create(), model: Sheet::from_csv(pathbuf).unwrap() }
    }
    pub fn run(&mut self){
        self.ui.update(&self.model)
        
    }
}
