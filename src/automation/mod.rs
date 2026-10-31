//! Terminal independent, synchronous sessions. Every command uses the worker's preparation path.
pub mod interaction;
pub mod registry;
use crate::{
    controller::{controller_impl::Controller, Command},
    model::database::Database,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, Write},
    path::PathBuf,
    sync::OnceLock,
};
static RECORD: OnceLock<PathBuf> = OnceLock::new();
pub fn set_record(path: Option<PathBuf>) {
    if let Some(path) = path {
        let _ = RECORD.set(path);
    }
}
pub(crate) fn recorder() -> crate::error::AppResult<Option<std::fs::File>> {
    if let Some(path) = RECORD.get() {
        let mut file = std::fs::File::create(path)?;
        writeln!(file, "{{\"kind\":\"header\",\"schema_version\":1}}")?;
        Ok(Some(file))
    } else {
        Ok(None)
    }
}
static PROFILE: OnceLock<String> = OnceLock::new();
pub fn set_profile(s: String) {
    let _ = PROFILE.set(s);
}
pub fn profile() -> &'static str {
    PROFILE.get().map(String::as_str).unwrap_or("visidata")
}
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    Header {
        schema_version: u32,
    },
    Context {
        sheet: String,
        row: usize,
        column: usize,
    },
    Command {
        name: String,
        #[serde(default)]
        inputs: Vec<String>,
    },
    Key {
        key: String,
    },
}
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Diagnostic {
    pub step: usize,
    pub phase: String,
    pub message: String,
    pub snapshot_error: Option<String>,
}
impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::to_string(self).unwrap())
    }
}
impl std::error::Error for Diagnostic {}
pub struct Session {
    controller: Controller,
    dispatcher: interaction::Dispatcher,
    records: Vec<Record>,
    ui: crate::controller::input::UiState,
    errors: Vec<Diagnostic>,
}
impl Session {
    pub fn load(paths: Vec<PathBuf>, profile: &str) -> Result<Self, String> {
        let db = Database::try_from(paths).map_err(|e| e.to_string())?;
        let mut controller = Controller::new(db).map_err(|e| e.to_string())?;
        controller.prepare_snapshot().map_err(|e| e.to_string())?;
        Ok(Self {
            controller,
            dispatcher: interaction::Dispatcher::new(profile, true),
            ui: Default::default(),
            errors: Vec::new(),
            records: vec![Record::Header { schema_version: 1 }],
        })
    }
    pub fn execute(&mut self, name: &str, inputs: Vec<String>) -> Result<(), Diagnostic> {
        let result = self.execute_inner(name, inputs);
        if let Err(error) = &result {
            self.errors.push(error.clone());
        }
        result
    }
    fn execute_inner(&mut self, name: &str, inputs: Vec<String>) -> Result<(), Diagnostic> {
        let step = self.records.len();
        let command = registry::resolve(name).map_err(|message| Diagnostic {
            step,
            phase: "prepare".into(),
            message,
            snapshot_error: None,
        })?;
        let (execution, snapshot) =
            self.controller
                .execute_and_snapshot(command, inputs.clone(), None);
        let snapshot_error = snapshot.err().map(|e| e.to_string());
        if let Err(e) = execution {
            return Err(Diagnostic {
                step,
                phase: "execution".into(),
                message: e.to_string(),
                snapshot_error,
            });
        }
        self.records.push(Record::Command {
            name: name.into(),
            inputs,
        });
        if let Some(message) = snapshot_error {
            return Err(Diagnostic {
                step,
                phase: "snapshot".into(),
                message,
                snapshot_error: None,
            });
        }
        Ok(())
    }
    pub fn context(&mut self, sheet: String, row: usize, column: usize) -> Result<(), String> {
        self.controller.automation_context(&sheet, row, column)?;
        self.records.push(Record::Context { sheet, row, column });
        Ok(())
    }
    pub fn write_snapshot(&mut self, out: impl Write) -> Result<(), String> {
        let extra = json!({"prompt":self.ui.editing.as_ref().map(|e|json!({"label":e.prompt(),"text":e.text,"cursor":e.cursor})),"pending_keys":self.dispatcher.pending(),"errors":self.errors});
        self.controller.automation_write_snapshot(out, extra)
    }
    pub fn snapshot(&mut self) -> Result<Value, String> {
        let mut value = self.controller.automation_snapshot()?;
        value["prompt"] = self
            .ui
            .editing
            .as_ref()
            .map(|e| json!({"label":e.prompt(),"text":e.text,"cursor":e.cursor}))
            .unwrap_or(Value::Null);
        value["pending_keys"] = json!(self.dispatcher.pending());
        value["errors"] = json!(self.errors);
        Ok(value)
    }
    pub fn key(&mut self, key: &str) -> Result<(), String> {
        let record_count = self.records.len();
        let event = interaction::parse_key(key)?;
        if event.code == crossterm::event::KeyCode::Tab {
            return Err("external editor unavailable in batch".into());
        }
        let command = if self.ui.editing.is_some() {
            if let Some((command, inputs)) =
                crate::controller::input::edit_prompt(&mut self.ui, event)
                    .map_err(|e| e.to_string())?
            {
                let name = registry::names()
                    .into_iter()
                    .find(|n| registry::resolve(n).ok().as_ref() == Some(&command))
                    .ok_or("unregistered command")?;
                self.execute(name, inputs).map_err(|e| e.to_string())?;
            }
            Command::None
        } else {
            self.dispatcher.key(event, &self.controller.view.table_name)
        };
        if command.needs_input() {
            self.ui.editing = Some(crate::controller::input::EditingSession::new(
                command.clone(),
                command.input_default(&self.controller.view.table_name),
            ));
        } else if command != Command::None && command != Command::HistoryClose {
            let name = registry::names()
                .into_iter()
                .find(|n| registry::resolve(n).ok().as_ref() == Some(&command))
                .ok_or("unregistered binding")?;
            self.execute(name, vec![]).map_err(|e| e.to_string())?;
        }
        self.records.truncate(record_count);
        self.records.push(Record::Key { key: key.into() });
        Ok(())
    }
    pub fn write_workflow(&self, mut out: impl Write) -> Result<(), String> {
        for r in &self.records {
            serde_json::to_writer(&mut out, r).map_err(|e| e.to_string())?;
            writeln!(out).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn replay(&mut self, input: impl BufRead) -> Result<(), Diagnostic> {
        let mut seen = false;
        for (index, line) in input.lines().enumerate() {
            seen = true;
            let fail = |phase: &str, message: String| Diagnostic {
                step: index + 1,
                phase: phase.into(),
                message,
                snapshot_error: None,
            };
            let record: Record =
                serde_json::from_str(&line.map_err(|e| fail("decode", e.to_string()))?)
                    .map_err(|e| fail("decode", e.to_string()))?;
            if index == 0 {
                if !matches!(record, Record::Header { schema_version: 1 }) {
                    return Err(fail("decode", "expected schema version 1 header".into()));
                }
                continue;
            }
            match record {
                Record::Header { .. } => return Err(fail("decode", "unexpected header".into())),
                Record::Context { sheet, row, column } => self
                    .context(sheet, row, column)
                    .map_err(|e| fail("context", e))?,
                Record::Command { name, inputs } => {
                    self.execute(&name, inputs).map_err(|mut e| {
                        e.step = index + 1;
                        e
                    })?
                }
                Record::Key { key } => {
                    self.key(&key).map_err(|e| fail("key", e))?;
                }
            }
        }
        if !seen {
            return Err(Diagnostic {
                step: 1,
                phase: "decode".into(),
                message: "missing workflow header".into(),
                snapshot_error: None,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.csv");
        std::fs::write(&path, "name,value\nsame,x\nsame,x\nα🙂,\n").unwrap();
        (dir, vec![path])
    }
    #[test]
    fn replay_validates_context_commands_and_stops_at_first_error() {
        let (_dir, paths) = fixture();
        for (text,phase) in [("{bad}","decode"),("{\"kind\":\"header\",\"schema_version\":2}","decode"),
            ("{\"kind\":\"header\",\"schema_version\":1}\n{\"kind\":\"command\",\"name\":\"unknown\"}","prepare"),
            ("{\"kind\":\"header\",\"schema_version\":1}\n{\"kind\":\"context\",\"sheet\":\"missing\",\"row\":0,\"column\":0}","context")]{
            let mut session=Session::load(paths.clone(),"visidata").unwrap();
            assert_eq!(session.replay(text.as_bytes()).unwrap_err().phase,phase);
            assert!(!session.snapshot().unwrap()["sheets"][0]["rows"][0]["selected"].as_bool().unwrap());
        }
        let mut session = Session::load(paths, "dataman").unwrap();
        assert!(session.execute("edit", vec![]).is_err());
        assert!(session.execute("sort", vec!["unexpected".into()]).is_err());
    }
    #[test]
    fn snapshots_are_full_deterministic_and_keep_duplicate_identity() {
        let (dir, paths) = fixture();
        let mut text = "name,value\n".to_owned();
        for _ in 0..257 {
            text.push_str("duplicate,x\n");
        }
        std::fs::write(&paths[0], text).unwrap();
        let mut session = Session::load(paths.clone(), "dataman").unwrap();
        session.context("rows".into(), 256, 1).unwrap();
        session.execute("edit", vec!["changed".into()]).unwrap();
        let first = session.snapshot().unwrap();
        assert_eq!(first, session.snapshot().unwrap());
        let mut stream = Vec::new();
        session.write_snapshot(&mut stream).unwrap();
        assert_eq!(first, serde_json::from_slice::<Value>(&stream).unwrap());
        let rows = first["sheets"][0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 257);
        assert_ne!(rows[0]["ref"], rows[1]["ref"]);
        assert!(!std::fs::read_to_string(&paths[0])
            .unwrap()
            .contains("changed"));
    }
    #[test]
    fn sqlite_cells_keep_null_empty_numbers_and_blob_and_source_is_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.sqlite");
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE typed(id INTEGER PRIMARY KEY, n, empty, i, f, b); INSERT INTO typed VALUES(1,NULL,'',42,1.5,X'00FF');").unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        let mut session = Session::load(vec![path.clone()], "dataman").unwrap();
        let state = session.snapshot().unwrap();
        let cells = &state["sheets"][0]["rows"][0]["cells"];
        assert_eq!(cells[1], json!("Null"));
        assert_eq!(cells[2], json!({"Text":""}));
        assert_eq!(cells[3], json!({"Integer":42}));
        assert_eq!(cells[4], json!({"Float":1.5}));
        assert_eq!(cells[5], json!({"Blob":[0,255]}));
        session.context("typed".into(), 0, 2).unwrap();
        session.execute("edit", vec!["modified".into()]).unwrap();
        assert_eq!(original, std::fs::read(path).unwrap());
    }
    #[test]
    fn prompt_editing_unicode_cancellation_and_key_record_replay() {
        let (_dir, paths) = fixture();
        let mut s = Session::load(paths.clone(), "dataman").unwrap();
        s.context("rows".into(), 0, 1).unwrap();
        s.key("e").unwrap();
        assert_eq!(s.snapshot().unwrap()["prompt"]["label"], "Value");
        for key in ["α", "🙂", "left", "backspace"] {
            s.key(key).unwrap();
        }
        assert_eq!(s.snapshot().unwrap()["prompt"]["text"], "🙂");
        let mut mutated = s.snapshot().unwrap();
        mutated["prompt"]["label"] = json!("Wrong prompt");
        assert_ne!(mutated, s.snapshot().unwrap());
        s.key("enter").unwrap();
        let mut log = Vec::new();
        s.write_workflow(&mut log).unwrap();
        let mut restored = Session::load(paths, "dataman").unwrap();
        restored.replay(log.as_slice()).unwrap();
        assert_eq!(s.snapshot().unwrap(), restored.snapshot().unwrap());
        s.key("e").unwrap();
        s.key("esc").unwrap();
        assert!(s.snapshot().unwrap()["prompt"].is_null());
    }
    #[test]
    fn dispatcher_sequences_cancel_and_legacy_profile_stays_available() {
        let mut d = interaction::Dispatcher::new("visidata", false);
        assert_eq!(
            d.key(interaction::parse_key("g").unwrap(), "rows"),
            Command::None
        );
        d.key(interaction::parse_key("esc").unwrap(), "rows");
        assert_eq!(
            d.key(interaction::parse_key("t").unwrap(), "rows"),
            Command::None
        );
        d.key(interaction::parse_key("g").unwrap(), "rows");
        assert_eq!(
            d.key(interaction::parse_key("t").unwrap(), "rows"),
            Command::InvertSelection
        );
        let mut legacy = interaction::Dispatcher::new("dataman", false);
        assert_eq!(
            legacy.key(interaction::parse_key("q").unwrap(), "rows"),
            Command::SqlQuery
        );
    }
}
