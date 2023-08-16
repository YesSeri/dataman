use crate::model::Sheet;

pub trait Display {
    fn update(&mut self, data: &Sheet);
    fn create() -> Self;
}

#[derive(Default)]
pub struct BasicUI { }


impl Display for BasicUI{
    fn update(&mut self, data: &Sheet) {
        let columns = data.columns.as_slice();
        for i in 0..columns.len() {
            let mut line:Vec<&str> = vec![];
            for column in columns {
                line.push(&column.data[i]);
            }
            let string = line.join(",");
            println!("{}", string);
        }
    }

    fn create() -> Self {
        Self{}
    }
}

