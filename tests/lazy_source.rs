use dataman::{
    controller::{controller_impl::Controller, Command},
    io::savers,
    model::database::{Database, RowCountState, StorageState},
};
use rusqlite::Connection;

fn fixture(sql: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source # %.sqlite");
    Connection::open(&path).unwrap().execute_batch(sql).unwrap();
    (directory, path)
}

#[test]
fn browsing_sorting_selecting_counting_and_exporting_do_not_promote() {
    let (directory, path) = fixture("CREATE TABLE t(id TEXT, value TEXT); INSERT INTO t VALUES('duplicate','b'),('duplicate','a'),(NULL,'c');");
    let before = std::fs::read(&path).unwrap();
    let mut db = Database::try_from(vec![path.clone()]).unwrap();
    assert_eq!(
        db.row_count_state("t"),
        RowCountState::Unknown { known_at_least: 0 }
    );
    let sheet = db.open_sheet("t").unwrap();
    let first = db.fetch_page(sheet, 2, 0, "").unwrap();
    assert!(first.has_more);
    assert_eq!(first.row_ids.len(), 2);
    assert_eq!(
        db.row_count_state("t"),
        RowCountState::Unknown { known_at_least: 3 }
    );
    assert_eq!(db.select_where(sheet, "value='a'").unwrap().len(), 1);
    assert!(
        db.fetch_page(sheet, 2, 0, "ORDER BY value")
            .unwrap()
            .has_more
    );
    assert_eq!(db.count_rows("t").unwrap(), 3);
    savers::csv(&db, "t", &directory.path().join("out.csv")).unwrap();
    savers::sqlite(&db, &directory.path().join("out.sqlite")).unwrap();
    assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn promotion_preserves_opaque_handles_for_native_locator_shapes() {
    for sql in [
        "CREATE TABLE t(value); INSERT INTO t(rowid,value) VALUES(-9223372036854775808,'a'),(9223372036854775807,'b');",
        "CREATE TABLE t(id TEXT, value); INSERT INTO t VALUES('duplicate','a'),('duplicate','b');",
        "CREATE TABLE t(id TEXT PRIMARY KEY, value) WITHOUT ROWID; INSERT INTO t VALUES('a','a'),('b','b');",
        "CREATE TABLE t(a TEXT,b TEXT,value,PRIMARY KEY(a,b)) WITHOUT ROWID; INSERT INTO t VALUES('a','x','a'),('b','y','b');",
        "CREATE TABLE t(rowid TEXT,_rowid_ TEXT,value); INSERT INTO t VALUES('shadow','shadow','a'),('shadow','shadow','b');",
    ] {
        let (_directory, path) = fixture(sql);
        let before = std::fs::read(&path).unwrap();
        let mut db = Database::try_from(vec![path.clone()]).unwrap();
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "").unwrap();
        let column = db.column_id(sheet, "value").unwrap();
        db.edit_row(sheet, page.row_ids[0], column, "edited").unwrap();
        assert_eq!(db.storage_state(), StorageState::PrivateWorkspace);
        assert_eq!(db.open_sheet("t").unwrap(), sheet);
        assert_eq!(db.fetch_page(sheet, 10, 0, "").unwrap().row_ids, page.row_ids);
        assert_eq!(db.cell_value(sheet, page.row_ids[0], column).unwrap().to_string(), "edited");
        db.edit_row(sheet, page.row_ids[1], column, "again").unwrap();
        assert!(savers::sqlite(&db, &path).is_err());
        drop(db);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn failed_first_mutation_keeps_a_valid_workspace_and_rolls_back() {
    let (_directory, path) = fixture(
        "CREATE TABLE t(value TEXT CHECK(value!='bad')); INSERT INTO t VALUES('original');",
    );
    let mut db = Database::try_from(vec![path]).unwrap();
    let sheet = db.open_sheet("t").unwrap();
    let row = db.fetch_page(sheet, 10, 0, "").unwrap().row_ids[0];
    let column = db.column_id(sheet, "value").unwrap();
    assert!(db.edit_row(sheet, row, column, "bad").is_err());
    assert_eq!(db.storage_state(), StorageState::PrivateWorkspace);
    assert_eq!(
        db.cell_value(sheet, row, column).unwrap().to_string(),
        "original"
    );
    db.edit_row(sheet, row, column, "good").unwrap();
}

#[test]
fn external_wal_commit_rejects_stale_edits_then_rediscovery_allocates_new_handles() {
    let (_directory, path) = fixture(
        "PRAGMA journal_mode=WAL; CREATE TABLE t(value); INSERT INTO t VALUES('original');",
    );
    let writer = Connection::open(&path).unwrap();
    let mut db = Database::try_from(vec![path]).unwrap();
    let sheet = db.open_sheet("t").unwrap();
    let row = db.fetch_page(sheet, 10, 0, "").unwrap().row_ids[0];
    let column = db.column_id(sheet, "value").unwrap();
    writer
        .execute_batch("DELETE FROM t; INSERT INTO t VALUES('replacement');")
        .unwrap();
    assert!(db
        .edit_row(sheet, row, column, "stale edit")
        .unwrap_err()
        .to_string()
        .contains("Source changed"));
    assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
    let fresh = db.open_sheet("t").unwrap();
    assert_ne!(sheet, fresh);
    assert!(db.cell_value(sheet, row, column).is_err());
    assert_eq!(
        db.fetch_page(fresh, 10, 0, "").unwrap().rows[0][0].to_string(),
        "replacement"
    );
}

#[test]
fn eof_inference_does_not_treat_an_arbitrary_empty_offset_as_a_total() {
    let (_directory, path) = fixture("CREATE TABLE t(value); INSERT INTO t VALUES(1),(2),(3),(4);");
    let mut db = Database::try_from(vec![path]).unwrap();
    let sheet = db.open_sheet("t").unwrap();
    assert!(!db.fetch_page(sheet, 2, 100, "").unwrap().has_more);
    assert_eq!(
        db.row_count_state("t"),
        RowCountState::Unknown { known_at_least: 0 }
    );
    assert!(db.fetch_page(sheet, 2, 0, "").unwrap().has_more);
    assert!(!db.fetch_page(sheet, 2, 2, "").unwrap().has_more);
    assert_eq!(db.row_count_state("t"), RowCountState::Exact(4));
}

#[test]
fn empty_and_final_partial_pages_infer_exact_counts() {
    for (inserts, total) in [("", 0), ("INSERT INTO t VALUES(1),(2),(3);", 3)] {
        let (_directory, path) = fixture(&format!("CREATE TABLE t(value); {inserts}"));
        let mut db = Database::try_from(vec![path]).unwrap();
        let sheet = db.open_sheet("T").unwrap();
        let page = db.fetch_page(sheet, 2, 0, "").unwrap();
        assert_eq!(page.has_more, total > 2);
        if page.has_more {
            let last = db.fetch_page(sheet, 2, 2, "").unwrap();
            assert_eq!(last.rows.len(), 1);
            assert!(!last.has_more);
        }
        assert_eq!(db.row_count_state("t"), RowCountState::Exact(total));
    }
}

#[test]
fn external_schema_changes_invalidate_old_columns_and_handles() {
    let (_directory, path) = fixture("CREATE TABLE t(value); INSERT INTO t VALUES('old');");
    let writer = Connection::open(&path).unwrap();
    let mut db = Database::try_from(vec![path]).unwrap();
    let sheet = db.open_sheet("t").unwrap();
    let row = db.fetch_page(sheet, 1, 0, "").unwrap().row_ids[0];
    let column = db.column_id(sheet, "value").unwrap();
    writer
        .execute_batch("ALTER TABLE t RENAME COLUMN value TO renamed")
        .unwrap();
    assert!(db.edit_row(sheet, row, column, "stale").is_err());
    let fresh = db.open_sheet("t").unwrap();
    assert_ne!(sheet, fresh);
    assert!(db.column_name(sheet, column).is_err());
    assert_eq!(
        db.fetch_page(fresh, 1, 0, "").unwrap().columns[0].name,
        "renamed"
    );
}

#[cfg(unix)]
#[test]
fn replacing_source_path_requires_reopening_and_never_retargets_handles() {
    let (directory, path) = fixture("CREATE TABLE t(value); INSERT INTO t VALUES('old');");
    let mut db = Database::try_from(vec![path.clone()]).unwrap();
    let sheet = db.open_sheet("t").unwrap();
    let page = db.fetch_page(sheet, 1, 0, "").unwrap();
    std::fs::rename(&path, directory.path().join("old.sqlite")).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t(value); INSERT INTO t VALUES('new');")
        .unwrap();
    assert!(db
        .fetch_page(sheet, 1, 0, "")
        .unwrap_err()
        .to_string()
        .contains("reopen"));
    assert_eq!(page.rows[0][0].to_string(), "old");
    assert!(db
        .open_sheet("t")
        .unwrap_err()
        .to_string()
        .contains("reopen"));
    assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
}

#[test]
fn export_rejects_hardlinks_to_the_source_after_promotion() {
    let (directory, path) = fixture("CREATE TABLE t(value); INSERT INTO t VALUES('old');");
    let alias = directory.path().join("alias.sqlite");
    std::fs::hard_link(&path, &alias).unwrap();
    let mut db = Database::try_from(vec![path]).unwrap();
    db.execute_batch("UPDATE t SET value='new'").unwrap();
    assert!(savers::sqlite(&db, &alias).is_err());
    assert!(savers::csv(&db, "t", &alias).is_err());
}

#[test]
fn view_snapshot_promotes_and_count_command_does_not_promote_or_enter_undo() {
    let (_directory, path) = fixture(
        "CREATE TABLE t(value); INSERT INTO t VALUES(1),(2); CREATE VIEW v AS SELECT value FROM t;",
    );
    let db = Database::try_from(vec![path.clone()]).unwrap();
    let mut controller = Controller::new(db).unwrap();
    controller.execute(Command::CountRows, vec![]).unwrap();
    controller.execute(Command::Undo, vec![]).unwrap();
    let mut db = Database::try_from(vec![path]).unwrap();
    let sheet = db.open_sheet("v").unwrap();
    assert_eq!(db.storage_state(), StorageState::PrivateWorkspace);
    assert!(db.sheet_name(sheet).unwrap().starts_with("v_snapshot"));
}
