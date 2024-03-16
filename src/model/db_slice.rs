use std::vec;

use crate::model::datarow::DataItem;
use ratatui::widgets::TableState;

#[derive(Debug)]
pub(crate) struct DatabaseSlice {
    pub(super) headers: Vec<String>,
    pub(super) data_rows: Vec<Vec<DataItem>>,
    pub(crate) table_state: TableState,
    pub(crate) row_offset: u32,
    pub(crate) is_unchanged: bool,
}

impl DatabaseSlice {
    pub(crate) fn new(
        headers: Vec<String>,
        data_rows: Vec<Vec<DataItem>>,
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

    pub(crate) fn column_widths(&mut self) -> Vec<u16> {
        let mut max_widths: Vec<u16> = self.headers.iter().map(|h| h.len() as u16).collect();
        for v in self.data_rows.iter() {
            for (i, item) in v.iter().enumerate() {
                let len = item.len() as u16;
                if len > max_widths[i] {
                    max_widths[i] = len;
                }
            }
        }
        // We trim stuff until all fits on screen
        // TODO maybe cache this?
        loop {
            if max_widths.iter().sum::<u16>() <= 92 {
                break;
            }
            let m: &mut u16 = max_widths
                .iter_mut()
                .fold(None, |acc, x| {
                    if let Some(acc) = acc {
                        if x > acc {
                            Some(x)
                        } else {
                            Some(acc)
                        }
                    } else {
                        Some(x)
                    }
                })
                .unwrap();
            *m -= 1;
        }
        max_widths
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
