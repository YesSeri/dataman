use ratatui::widgets::Cell;
use std::collections::VecDeque;
use std::fmt::Display;

use rusqlite::types::ValueRef;
use rusqlite::Row;
use serde::Serialize;

pub type DataTable = (Vec<String>, Vec<Vec<DataItem>>);

impl Display for DataItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = self.clone().into();
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub enum DataItem {
    Text(String),
    Integer(i64),
    Float(f64),
    Null,
}
impl DataItem {
    pub fn len(&self) -> usize {
        match self {
            DataItem::Text(s) => s.len(),
            DataItem::Integer(x) => x.to_string().len(),
            DataItem::Float(x) => x.to_string().len(),
            DataItem::Null => 4,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0 || self == &DataItem::Null
    }
}

impl<'a> From<DataItem> for Cell<'a> {
    fn from(value: DataItem) -> Self {
        match value {
            DataItem::Text(text) => Cell::from(text.clone()),
            DataItem::Integer(int) => Cell::from(int.to_string()),
            DataItem::Float(float) => Cell::from(float.to_string()),
            DataItem::Null => Cell::from("NULL"),
        }
    }
}

impl From<DataItem> for String {
    fn from(item: DataItem) -> String {
        match item {
            DataItem::Text(text) => text,
            DataItem::Integer(num) => num.to_string(),
            DataItem::Float(num) => num.to_string(),
            DataItem::Null => String::from(""),
        }
    }
}

impl<'a> From<ValueRef<'a>> for DataItem {
    fn from(value: ValueRef) -> Self {
        match value {
            ValueRef::Null => DataItem::Null,
            ValueRef::Integer(n) => DataItem::Integer(n),
            ValueRef::Real(float) => DataItem::Float(float),
            ValueRef::Text(s) => DataItem::Text(String::from_utf8_lossy(s).to_string()),
            ValueRef::Blob(_) => unimplemented!("blob"),
        }
    }
}
