use std::{fs::File, path::PathBuf};

use csv::{Error, ReaderBuilder};

use crate::error::AppError;
use crate::libstuff::db::Database;

#[derive(Debug)]
pub struct Column {
    pub data: Vec<String>,
}

impl std::fmt::Debug for Sheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut res = String::new();
        for col in self.columns.iter() {
            res.push_str(&format!("\r\n{:?}\r\n", col));
        }
        write!(f, "\r\n{}\r\n", res)
    }
}

impl Column {
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
    pub columns: Vec<Column>,
    pub cursor: usize,
    pub user_input: String,
    pub mode: Mode,
}

impl From<Database> for Vec<Sheet> {
    fn from(db: Database) -> Self {
        for table_name in db.table_names {
            // select all from table_name
            let query = format!("SELECT * FROM {}", table_name);
            let mut stmt = db.connection.prepare(&query).unwrap();
            let cols = stmt.column_names().iter().map(|s| s.to_string()).collect::<Vec<String>>();

            let mut rows = stmt.query([]).unwrap();

            while let Ok(row) = rows.next() {
                if let Some(row) = row {
                    // print each field in the row
                    let mut i = 0;
                    // print column names
                    while let Ok(field) = row.get::<usize, String>(i) {
                        // print column name and field name

                        println!("{} {}", cols[i], field);
                        i += 1;
                    }
                }
            }


            todo!();


            Sheet::new(vec![]);
        }
        todo!();
    }
}

impl TryFrom<&str> for Sheet {
    type Error = AppError;

    fn try_from(data: &str) -> Result<Self, Self::Error> {
        let mut reader = ReaderBuilder::new()
            .has_headers(false)
            .from_reader(data.as_bytes());

        let first = reader.records().next();
        if let Some(Ok(header)) = first {
            let len = header.len();
            let mut columns = vec![];
            for header in header.iter() {
                columns.push(Column::new(vec![header.to_string()]));
            }
            for result in reader.records() {
                let result = result?;
                // add one column per record
                for (i, data) in result.into_iter().enumerate() {
                    let d = data.to_string();
                    columns[i].data.push(d);
                }
            }
            Ok(Sheet::new(columns))
        } else {
            Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "No data found",
            )))
        }
    }
}

impl Sheet {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            cursor: 0,
            user_input: "".to_string(),
            mode: Mode::Normal,
        }
    }
    pub fn get(&self, x: usize, y: usize) -> String {
        self.columns[x].data[y].clone()
    }
    pub fn change_mode(&mut self, mode: Mode) {
        self.user_input.clear();
        self.mode = mode;
    }

    pub fn from_csv(file_path: &PathBuf) -> Result<Sheet, std::io::Error> {
        let file = File::open(file_path)?;
        let mut reader = ReaderBuilder::new().has_headers(false).from_reader(file);

        let first = reader.records().next().unwrap()?;
        let len = first.len();
        let mut columns = vec![];
        for header in first.iter() {
            columns.push(Column::new(vec![header.to_string()]));
        }

        for result in reader.records() {
            let result = result?;
            // add one column per record
            for (i, data) in result.into_iter().enumerate() {
                let d = data.to_string();
                columns[i].data.push(d);
            }
        }
        Ok(Sheet::new(columns))
    }

    pub fn derive_new(&mut self, fun: impl Fn(String) -> String) {
        let i = self.cursor;
        let mut res = vec![];
        let col = &self.columns[i];
        let header = format!("{}-DER", col.data[0]);
        res.push(header);
        for d in col.get_data().iter() {
            let transformed_data = fun(d.to_string());
            res.push(transformed_data);
        }
        let new_col = Column::new(res);
        self.columns.push(new_col);
    }
}

#[cfg(test)]
mod test {}
