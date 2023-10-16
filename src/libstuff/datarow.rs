use rusqlite::types::ValueRef;
use rusqlite::Row;

pub type DataTable = (Vec<String>, Vec<DataRow>);

#[derive(Debug, Clone)]
pub struct DataRow {
    pub data: Vec<DataItem>,
}

impl DataRow {
    pub(crate) fn get(&self, i: i32) -> DataItem {
        self.data.get(i as usize).unwrap_or(&DataItem::Null).clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DataItem {
    Text(String),
    Integer(i64),
    Null,
}

impl From<DataItem> for String {
    fn from(item: DataItem) -> String {
        match item {
            DataItem::Text(text) => text,
            DataItem::Integer(num) => num.to_string(),
            DataItem::Null => String::from("NULL"),
        }
    }
}

impl From<Vec<&str>> for DataRow {
    fn from(value: Vec<&str>) -> Self {
        let mut items = Vec::new();
        for item in value {
            items.push(DataItem::Text(item.to_string()));
        }
        Self { data: items }
    }
}

impl<'a> From<&Row<'a>> for DataRow {
    fn from(value: &Row) -> Self {
        let mut items = Vec::new();
        let mut i = 0;
        while let Ok(field) = value.get_ref(i) {
            items.push(match field {
                ValueRef::Null => DataItem::Null,
                ValueRef::Integer(cell) => DataItem::Integer(cell),
                ValueRef::Real(_) => unimplemented!("real"),
                ValueRef::Text(cell) => DataItem::Text(String::from_utf8_lossy(cell).to_string()),
                ValueRef::Blob(_) => unimplemented!("blob"),
            });
            i += 1;
        }
        Self { data: items }
    }
}

impl DataRow {
    pub(crate) fn to_tui_row(&self) -> ratatui::widgets::Row {
        ratatui::widgets::Row::new(self.data.iter().map(|item| match item {
            DataItem::Text(text) => ratatui::widgets::Cell::from(text.clone()),
            DataItem::Integer(int) => ratatui::widgets::Cell::from(int.to_string()),
            DataItem::Null => ratatui::widgets::Cell::from("NULL"),
        }))
    }
}
