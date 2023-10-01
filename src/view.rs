use std::{
    fmt::Debug,
    io::{self, Stdout, Write},
    thread,
    time::Duration,
};

use crate::error::AppError;

use crossterm::{
    cursor::position,
    event::{self, poll, read, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute, queue,
    terminal::{self, enable_raw_mode},
    ExecutableCommand, QueueableCommand,
};
use ratatui::{
    prelude::CrosstermBackend,
    widgets::{Block, Borders},
    Terminal,
};

pub trait Display {
    fn update(&mut self, headers: Vec<String>, rows: Vec<Vec<String>>) -> Result<(), AppError>;
    // fn update_input(&mut self, sheet: &Sheet) -> Result<(), AppError>;
    fn new() -> Self;
    fn shutdown(&mut self) -> Result<(), AppError>;
}

#[derive(Debug)]
pub struct BasicUI {
    stdout: Stdout,
}

impl Display for BasicUI {
    fn update(&mut self, headers: Vec<String>, rows: Vec<Vec<String>>) -> Result<(), AppError> {
        // let columns = sheet.rows.as_slice();
        // let mut res = vec![];
        // let selected_column = sheet.cursor;

        // //        self.stdout
        // //            .execute()
        // //            .unwrap();
        // queue!(
        //     io::stdout(),
        //     terminal::Clear(terminal::ClearType::All),
        //     crossterm::cursor::MoveTo(0, 0),
        // )?;

        // for i in 0..columns[0].data.len() {
        //     let mut row = vec![];
        //     for (j, column) in columns.iter().enumerate() {
        //         if i == 0 && j == sheet.cursor {
        //             let data = format!("**{}**", column.data[i]);
        //             row.push(format!("{:>20}", data));
        //         } else {
        //             row.push(format!("{:>20}", column.data[i]));
        //         }
        //     }
        //     res.push(row.join(""));
        // }
        // print!("{}\n\r{}\r\n", selected_column, res.join("\r\n"));

        // self.stdout.flush()?;
        Ok(())
    }
    fn new() -> Self {
        terminal::enable_raw_mode().unwrap();

        let mut stdout = io::stdout();
        execute!(stdout, DisableMouseCapture).unwrap();

        Self { stdout }
    }
    fn shutdown(&mut self) -> Result<(), AppError> {
        terminal::disable_raw_mode()?;
        Ok(())
    }

    // fn update_input(&mut self, sheet: &Sheet) -> Result<(), AppError> {
    // if let Ok((cols, rows)) = crossterm::terminal::size() {
    //     queue!(
    //         io::stdout(),
    //         crossterm::cursor::MoveTo(0, rows - 1),
    //         crossterm::cursor::EnableBlinking,
    //         terminal::Clear(terminal::ClearType::All)
    //     )?;
    //     let msg = match sheet.mode {
    //         Mode::Normal => "*normal*".to_string(),
    //         Mode::Regex => "<regex>:".to_string(),
    //         Mode::RegexReplace => "(replace):".to_string(),
    //     };

    //     print!("{}{}", msg, sheet.user_input);
    // }
    // self.stdout.flush()?;
    //     Ok(())
    // }
}

#[derive(Debug)]
pub struct DebugUI {
    stdout: Stdout,
}

impl Display for DebugUI {
    fn update(&mut self, headers: Vec<String>, rows: Vec<Vec<String>>) -> Result<(), AppError> {
        todo!();
    }
    fn new() -> Self {
        let stdout = io::stdout();

        Self { stdout }
    }
    fn shutdown(&mut self) -> Result<(), AppError> {
        todo!()
    }
}
