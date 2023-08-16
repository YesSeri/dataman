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
    pub fn derive_new(column: &Column, fun: fn(String) -> String) -> Self {
        let mut data = vec![];
        let header = format!("{}-derived", column.data[0]);
        data.push(header);
        for d in column.get_data().iter() {
            data.push(fun(d.to_string()));
        }
        Self::new(data)
    }
}

#[derive(Debug)]
pub struct Sheet {
    pub columns: Vec<Column>,
    pub cursor: usize,
}
impl Sheet {
    pub fn new(columns: Vec<Column>, cursor: usize) -> Self {
        Self {
            columns,
            cursor,
        }
    }
    pub fn get(&self, x: usize, y: usize) -> String {
        self.columns[x].data[y].clone()
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
//    }
}
