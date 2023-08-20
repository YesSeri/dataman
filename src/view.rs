use std::{
    fmt::Debug,
    io::{self, Stdout, Write},
    time::Duration,
};

use crate::model::{Mode, Sheet};
use crossterm::{queue, ExecutableCommand, QueueableCommand};

use crossterm::{
    cursor::position,
    event::{poll, read, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{self, disable_raw_mode, enable_raw_mode},
};
pub trait Display {
    fn update(&mut self, sheet: &Sheet) -> Result<(), std::io::Error>;
    fn new() -> Self;
    fn shutdown(&self) -> Result<(), std::io::Error>;
}

pub struct BasicUI {
    stdout: Stdout,
}

impl Display for BasicUI {
    fn update(&mut self, sheet: &Sheet) -> Result<(), std::io::Error> {
        let columns = sheet.columns.as_slice();
        let mut res = vec![];
        let selected_column = sheet.cursor;

        //        self.stdout
        //            .execute()
        //            .unwrap();
        queue!(
            io::stdout(),
            terminal::Clear(terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0),
        )?;

        for i in 0..columns[0].data.len() {
            let mut row = vec![];
            for j in 0..columns.len() {
                if i == 0 && j == sheet.cursor {
                    let data = format!("**{}**", columns[j].data[i].clone());
                    row.push(format!("{:>20}", data));
                } else {
                    row.push(format!("{:>20}", columns[j].data[i].clone()));
                }
            }
            res.push(row.join(","));
        }
        print!("{}\n\r{}\r\n", selected_column, res.join("\r\n"));

        if let Ok((cols, rows)) = crossterm::terminal::size() {
            queue!(
                io::stdout(),
                crossterm::cursor::MoveTo(0, rows - 1),
                crossterm::cursor::EnableBlinking,
            )?;
            let msg = match sheet.mode {
                Mode::Normal => "*normal*".to_string(),
                Mode::Regex => "regex:".to_string(),
            };
            print!(
                "{}{}",
                msg,
                sheet.user_inputs.last().unwrap_or(&"".to_string())
            );
        }
        self.stdout.flush()?;
        Ok(())
    }
    fn new() -> Self {
        enable_raw_mode().unwrap();

        let mut stdout = io::stdout();
        execute!(stdout, DisableMouseCapture).unwrap();

        Self { stdout }
    }
    fn shutdown(&self) -> Result<(), std::io::Error> {
        disable_raw_mode()?;
        Ok(())
    }
}

pub struct DebugUI {
    stdout: Stdout,
}

impl Display for DebugUI {
    fn update(&mut self, sheet: &Sheet) -> Result<(), std::io::Error> {
        todo!();
    }
    fn new() -> Self {
        let stdout = io::stdout();

        Self { stdout }
    }
    fn shutdown(&self) -> Result<(), std::io::Error> {
        todo!()
    }
}
