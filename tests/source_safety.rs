use dataman::{controller::controller_impl::Controller, io::savers, model::database::Database};
use rusqlite::Connection;

#[test]
fn browsing_editing_exporting_and_exit_preserve_sqlite_source_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let destination = directory.path().join("edited.db");
    {
        let connection = Connection::open(&source).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE data(id INTEGER PRIMARY KEY, value TEXT, __dataman_undo_user TEXT);
             INSERT INTO data VALUES(1, 'original', 'keep');
             CREATE TABLE table_of_tables(message TEXT);
             INSERT INTO table_of_tables VALUES('user catalog');
             CREATE TABLE __dataman_catalog(message TEXT);
             INSERT INTO __dataman_catalog VALUES('also user data');
             CREATE TABLE __dataman_undo_table_0(value TEXT);
             INSERT INTO __dataman_undo_table_0 VALUES('never delete');
             CREATE TABLE composite(a TEXT, b TEXT, value TEXT, PRIMARY KEY(a,b)) WITHOUT ROWID;
             INSERT INTO composite VALUES('x','y','preserve');
             CREATE VIEW user_view AS SELECT value FROM data;",
            )
            .unwrap();
    }
    let before = std::fs::read(&source).unwrap();
    let mut database = Database::try_from(vec![source.clone()]).unwrap();
    let names = database.get_table_names().unwrap();
    for name in [
        "table_of_tables",
        "__dataman_catalog",
        "__dataman_undo_table_0",
        "composite",
    ] {
        assert!(names.iter().any(|candidate| candidate == name));
    }
    assert_eq!(
        database.get(10, 0, "data", "ORDER BY id").unwrap().1.len(),
        1
    );
    database
        .update_cell("data", "value", 1, "workspace edit")
        .unwrap();
    assert!(savers::sqlite(&database, &source).is_err());
    assert!(savers::csv(&database, "data", &source).is_err());
    savers::sqlite(&database, &destination).unwrap();
    drop(database);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let exported = Connection::open(destination).unwrap();
    let value: String = exported
        .query_row("SELECT value FROM data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "workspace edit");
    for query in [
        "SELECT message FROM table_of_tables",
        "SELECT message FROM __dataman_catalog",
        "SELECT value FROM __dataman_undo_table_0",
        "SELECT value FROM composite",
        "SELECT value FROM user_view",
    ] {
        assert!(exported
            .query_row(query, [], |row| row.get::<_, String>(0))
            .is_ok());
    }
    assert_eq!(
        exported
            .query_row("SELECT __dataman_undo_user FROM data", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}

#[test]
fn opening_a_missing_sqlite_source_does_not_create_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.sqlite");
    assert!(Database::try_from(vec![path.clone()]).is_err());
    assert!(!path.exists());
}

#[test]
fn startup_browses_tables_without_id_and_handles_empty_databases() {
    for ddl in [
        "",
        "CREATE TABLE data(value TEXT); INSERT INTO data VALUES('same'),('same');",
    ] {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(ddl).unwrap();
        let database = Database::new(connection).unwrap();
        let mut controller = Controller::new(database).unwrap();
        let (_, rows) = controller.get_headers_and_rows(10).unwrap();
        assert_eq!(rows.len(), if ddl.is_empty() { 0 } else { 2 });
    }
}

#[test]
fn delimited_export_preserves_user_id_and_reserved_looking_columns() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE data(id TEXT, __dataman_undo_value TEXT); INSERT INTO data VALUES('001', 'unicode: æøå');").unwrap();
    let database = Database::new(connection).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("export.csv");
    savers::csv(&database, "data", &path).unwrap();
    let mut reader = csv::Reader::from_path(path).unwrap();
    assert_eq!(
        reader.headers().unwrap(),
        &csv::StringRecord::from(vec!["id", "__dataman_undo_value"])
    );
    assert_eq!(
        reader.records().next().unwrap().unwrap(),
        csv::StringRecord::from(vec!["001", "unicode: æøå"])
    );
}

#[test]
fn delimited_round_trip_preserves_duplicate_text_ids_and_empty_strings() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.csv");
    let output = directory.path().join("output.csv");
    std::fs::write(&input, "id,__dataman_undo_user\n001,\n001,æøå\n").unwrap();
    let mut database = Database::try_from(vec![input.clone()]).unwrap();
    assert_eq!(database.get(10, 0, "input", "").unwrap().1.len(), 2);
    // The integer-key compatibility helper rejects ambiguous writes; opaque
    // handle editing is covered by the generic identity integration suite.
    assert!(database
        .update_cell("input", "__dataman_undo_user", 1, "wrong row")
        .is_err());
    savers::csv(&database, "input", &output).unwrap();
    let records = |path| {
        csv::Reader::from_path(path)
            .unwrap()
            .records()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(records(&input), records(&output));
}

#[test]
fn snapshots_include_uncheckpointed_wal_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wal.db");
    let writer = Connection::open(&path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE data(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO data VALUES(1, 'in WAL');").unwrap();
    let mut database = Database::try_from(vec![path]).unwrap();
    assert_eq!(database.get_cell("data", 1, "value").unwrap(), "in WAL");
    database.update_cell("data", "value", 1, "private").unwrap();
    assert_eq!(
        writer
            .query_row("SELECT value FROM data", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "in WAL"
    );
}
