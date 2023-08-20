use std::{error::Error, fs::File, path::PathBuf};

use csv::ReaderBuilder;

#[derive(Debug)]
pub struct Column {
    pub data: Vec<String>,
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
    Normal,
}
impl Default for Mode {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Debug)]
pub struct Sheet {
    pub columns: Vec<Column>,
    pub cursor: usize,
    pub user_inputs: Vec<String>,
    pub mode: Mode,
}
impl Sheet {
    pub fn new(columns: Vec<Column>, cursor: usize) -> Self {
        Self {
            columns,
            cursor,
            user_inputs: vec![],
            mode: Mode::Normal,
        }
    }
    pub fn get(&self, x: usize, y: usize) -> String {
        self.columns[x].data[y].clone()
    }
    pub fn change_mode(&mut self, mode: Mode){
        self.user_inputs.push(String::new());
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
        Ok(Sheet::new(columns, 0))
    }

    pub fn derive_new(&mut self, i: usize, fun: impl Fn(String) -> String) {
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
mod test {
    use super::*;

    //    #[test]
    //    fn derive() {
    //        let file_path = "assets/data.csv";
    //        let mut app = App::from_csv(file_path).unwrap();
    //        assert_eq!(app.editor.sheet.columns.len(), 3);
    //        app.derive(1, |cell| format!("{}X{}", cell, cell));
    //
    //        let text = app.editor.sheet.get(1, 1);
    //        assert_eq!("zenkert".to_string(), text);
    //
    //        let text = app.editor.sheet.get(3, 1);
    //        assert_eq!("zenkertXzenkert".to_string(), text);
    //        assert_eq!(app.editor.sheet.columns.len(), 4);
//    //    }
}
