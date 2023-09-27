use std::{fs::File, path::PathBuf};
use std::io::Read;

use csv::{Error, ReaderBuilder};
use rusqlite::types::ValueRef;

use crate::error::AppError;
use crate::libstuff::db::Database;

#[derive(Debug)]
pub struct Row {
    pub data: Vec<String>,
}

impl std::fmt::Debug for Sheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut res = String::new();
        for col in self.rows.iter() {
            res.push_str(&format!("\r\n{:?}\r\n", col));
        }
        write!(f, "\r\n{}\r\n", res)
    }
}

impl Row {
    pub fn new(data: Vec<String>) -> Self {
        Self { data }
    }
    pub fn get_data(&self) -> &[String] {
        &self.data[1..]
    }
}

#[derive(PartialEq, Debug)]
pub enum Mode {
    Regex,
    RegexReplace,
    Normal,
}

impl Default for Mode {
    fn default() -> Self {
        Self::Normal
    }
}

pub struct Sheet {
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub user_input: String,
    pub mode: Mode,
}

impl Sheet {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            cursor: 0,
            user_input: "".to_string(),
            mode: Mode::Normal,
        }
    }
    pub fn get(&self, x: usize, y: usize) -> String {
        self.rows[x].data[y].clone()
    }
    pub fn change_mode(&mut self, mode: Mode) {
        self.user_input.clear();
        self.mode = mode;
    }


    pub fn derive_new(&mut self, fun: impl Fn(String) -> String) {
        let i = self.cursor;
        let mut res = vec![];
        let col = &self.rows[i];
        let header = format!("{}-DER", col.data[0]);
        res.push(header);
        for d in col.get_data().iter() {
            let transformed_data = fun(d.to_string());
            res.push(transformed_data);
        }
        let new_col = Row::new(res);
        self.rows.push(new_col);
    }
}

#[cfg(test)]
mod test {}
