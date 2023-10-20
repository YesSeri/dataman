use ratatui::widgets::TableState;

use super::datarow::DataRow;

#[derive(Debug)]
pub(crate) struct CurrentView {
    pub(super) headers: Vec<String>,
    pub(super) data_rows: Vec<DataRow>,
    pub(crate) table_state: TableState,
    pub(crate) row_offset: u32,
    pub(crate) is_unchanged: bool,
}

impl CurrentView {
    pub(crate) fn new(
        headers: Vec<String>,
        data_rows: Vec<DataRow>,
        table_state: TableState,
        row_idx: u32,
        row_offset: u32,
    ) -> Self {
        Self {
            headers,
            data_rows,
            table_state,
            row_offset,
            is_unchanged: false,
        }
    }
    pub(crate) fn update(&mut self, row_idx: u32, row_offset: u32) {
        self.table_state.select(Some(row_idx as usize));
        self.row_offset = row_offset;
        self.is_unchanged = false;
    }
    pub(crate) fn is_unchanged(&self) -> bool {
        self.is_unchanged
    }

    pub(crate) fn has_changed(&mut self) {
        self.is_unchanged = false;
    }
}
