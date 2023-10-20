use ratatui::widgets::TableState;

use super::datarow::DataRow;

#[derive(Debug)]
pub(crate) struct CurrentView {
    pub(super) headers: Vec<String>,
    pub(super) data_rows: Vec<DataRow>,
    pub(crate) table_state: TableState,
    pub(crate)  row_idx: u32,
    pub(crate) row_offset: u32,
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
            row_idx,
            row_offset,
        }
    }
}
