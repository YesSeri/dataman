use ratatui::widgets::Cell;
use std::collections::VecDeque;
use std::fmt::Display;

use rusqlite::types::ValueRef;
use rusqlite::Row;
use serde::Serialize;

pub type DataTable = (Vec<String>, Vec<Vec<DataItem>>);

// pub type DRow = Vec<DataItem>;

// #[derive(Debug, Clone, Serialize)]
// pub struct DataRow {
//     pub data: VecDeque<DataItem>,
// }

impl Display for DataItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = self.clone().into();
        write!(f, "{}", s)
    }
}

// impl Iterator for DataRow {
//     type Item = DataItem;
//
//     fn next(&mut self) -> Option<Self::Item> {
//         self.data.pop_front()
//     }
// }

#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub enum DataItem {
    Text(String),
    Integer(i64),
    Null,
}
impl DataItem {
    pub fn len(&self) -> usize {
        match self {
            DataItem::Text(s) => s.len(),
            DataItem::Integer(x) => x.to_string().len(),
            DataItem::Null => 4,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a> From<DataItem> for Cell<'a> {
    fn from(value: DataItem) -> Self {
        match value {
            DataItem::Text(text) => Cell::from(text.clone()),
            DataItem::Integer(int) => Cell::from(int.to_string()),
            DataItem::Null => Cell::from("NULL"),
        }
    }
}

impl From<DataItem> for String {
    fn from(item: DataItem) -> String {
        match item {
            DataItem::Text(text) => text,
            DataItem::Integer(num) => num.to_string(),
            DataItem::Null => String::from(""),
        }
    }
}

// impl From<Vec<&str>> for DataRow {
//     fn from(value: Vec<&str>) -> Self {
//         let mut items = VecDeque::new();
//         for item in value {
//             items.push_back(DataItem::Text(item.to_string()));
//         }
//         Self { data: items }
//     }
// }

impl<'a> From<ValueRef<'a>> for DataItem {
    fn from(value: ValueRef) -> Self {
        // let mut items = VecDeque::new();
        // let mut i = 0;
        // while let Ok(field) = value.get_ref(i) {
        match value {
            ValueRef::Null => DataItem::Null,
            ValueRef::Integer(n) => DataItem::Integer(n),
            ValueRef::Real(_) => unimplemented!("real"),
            ValueRef::Text(s) => DataItem::Text(String::from_utf8_lossy(s).to_string()),
            ValueRef::Blob(_) => unimplemented!("blob"),
        }
        //     i += 1;
        // }
        // Self { data: items }
    }
}
