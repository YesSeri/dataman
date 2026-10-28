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
    Blob(Vec<u8>),
    Null,
}
impl DataItem {
    pub fn len(&self) -> usize {
        match self {
            DataItem::Text(s) => s.len(),
            DataItem::Integer(x) => x.to_string().len(),
            DataItem::Float(x) => x.to_string().len(),
            DataItem::Blob(bytes) => bytes.len() * 2,
            DataItem::Null => 4,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0 || self == &DataItem::Null
    }
}

impl From<DataItem> for String {
    fn from(item: DataItem) -> String {
        match item {
            DataItem::Text(text) => text,
            DataItem::Integer(num) => num.to_string(),
            DataItem::Float(num) => num.to_string(),
            DataItem::Blob(bytes) => bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
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
            ValueRef::Blob(bytes) => DataItem::Blob(bytes.to_vec()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_values_are_supported() {
        let item = DataItem::from(ValueRef::Blob(&[0x00, 0xab, 0xff]));
        assert_eq!(item, DataItem::Blob(vec![0x00, 0xab, 0xff]));
        assert_eq!(item.to_string(), "00abff");
    }
}
