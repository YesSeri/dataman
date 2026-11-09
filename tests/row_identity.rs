use dataman::{
    controller::{controller_impl::Controller, Command},
    io::savers,
    model::{database::Database, datarow::DataItem},
    navigation::Direction,
};
use rusqlite::Connection;

#[test]
fn generic_identity_edits_and_exports_never_change_source_files() {
    let fixtures = [
        "CREATE TABLE data(value TEXT); INSERT INTO data VALUES('first'),('second');",
        "CREATE TABLE data(id TEXT, value TEXT); INSERT INTO data VALUES('001','first'),('001','second');",
        "CREATE TABLE data(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO data VALUES(-9223372036854775808,'first'),(9223372036854775807,'second');",
        "CREATE TABLE data(key TEXT PRIMARY KEY, value TEXT) WITHOUT ROWID; INSERT INTO data VALUES('a','first'),('b','second');",
        "CREATE TABLE data(a TEXT COLLATE NOCASE, b BLOB, value TEXT, PRIMARY KEY(a,b)) WITHOUT ROWID; INSERT INTO data VALUES('A',X'00FF','first'),('B',X'0100','second');",
        "CREATE TABLE data(rowid TEXT, _rowid_ TEXT, oid TEXT, value TEXT); INSERT INTO data VALUES('x','x','x','first'),('x','x','x','second');",
        "CREATE TABLE data(rowid TEXT, _rowid_ TEXT, oid TEXT, id INTEGER PRIMARY KEY, value TEXT); INSERT INTO data VALUES('x','x','x',1,'first'),('x','x','x',2,'second');",
        "CREATE TABLE backing(value TEXT); INSERT INTO backing VALUES('first'),('second'); CREATE VIEW data AS SELECT value FROM backing;",
    ];
    for ddl in fixtures {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let output = directory.path().join("copy.db");
        {
            let connection = Connection::open(&source).unwrap();
            connection.execute_batch(ddl).unwrap();
        }
        let before = std::fs::read(&source).unwrap();
        let mut database = Database::try_from(vec![source.clone()]).unwrap();
        let sheet = database.open_sheet("data").unwrap();
        let name = database.sheet_name(sheet).unwrap();
        let page = database.fetch_page(sheet, 10, 0, "").unwrap();
        assert_eq!(page.row_ids.len(), 2);
        assert_ne!(page.row_ids[0], page.row_ids[1]);
        let column = database.column_id(sheet, "value").unwrap();
        database
            .edit_row(sheet, page.row_ids[1], column, "changed")
            .unwrap();
        assert_eq!(
            database.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Text("first".into())
        );
        assert_eq!(
            database.cell_value(sheet, page.row_ids[1], column).unwrap(),
            DataItem::Text("changed".into())
        );
        savers::sqlite(&database, &output).unwrap();
        let exported = Connection::open(output).unwrap();
        let query = format!(
            "SELECT COUNT(*) FROM \"{}\" WHERE value='changed'",
            name.replace('"', "\"\"")
        );
        assert_eq!(
            exported
                .query_row(&query, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(exported);
        drop(database);
        assert_eq!(
            std::fs::read(source).unwrap(),
            before,
            "source changed for {ddl}"
        );
    }
}

#[test]
fn controller_edits_bulk_deletes_undoes_and_exports_duplicate_user_ids() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE data(id TEXT, value TEXT); INSERT INTO data VALUES('001','first'),('001','second');").unwrap();
    let database = Database::new(connection).unwrap();
    let mut controller = Controller::new(database).unwrap();
    controller
        .execute(Command::Move(Direction::Right), vec![])
        .unwrap();
    controller
        .execute(Command::SelectSql, vec!["value='first'".into()])
        .unwrap();
    controller
        .execute(Command::BulkEdit, vec!["bulk".into()])
        .unwrap();
    let (_, rows) = controller.get_headers_and_rows(10).unwrap();
    assert_eq!(
        rows[0],
        vec![DataItem::Text("001".into()), DataItem::Text("bulk".into())]
    );
    assert_eq!(rows[1][1], DataItem::Text("second".into()));
    controller.execute(Command::Undo, vec![]).unwrap();
    assert_eq!(
        controller.get_headers_and_rows(10).unwrap().1[0][1],
        DataItem::Text("first".into())
    );
    controller.execute(Command::Redo, vec![]).unwrap();
    controller.execute(Command::DeleteSelected, vec![]).unwrap();
    assert_eq!(controller.get_headers_and_rows(10).unwrap().1.len(), 1);
    controller.execute(Command::Undo, vec![]).unwrap();
    assert_eq!(controller.get_headers_and_rows(10).unwrap().1.len(), 2);
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("edited.csv");
    controller
        .execute(Command::Save, vec![output.to_str().unwrap().to_owned()])
        .unwrap();
    let mut csv = csv::Reader::from_path(output).unwrap();
    assert_eq!(
        csv.headers().unwrap(),
        &csv::StringRecord::from(vec!["id", "value"])
    );
    assert_eq!(csv.records().count(), 2);
}

#[test]
fn csv_input_and_database_export_contain_only_user_columns() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.csv");
    let output = directory.path().join("output.db");
    std::fs::write(&source, "__dataman_undo_user,value\n001,\n001,æøå\n").unwrap();
    let mut database = Database::try_from(vec![source]).unwrap();
    let sheet = database.open_sheet("input").unwrap();
    let page = database.fetch_page(sheet, 10, 0, "").unwrap();
    assert_eq!(
        page.columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec!["__dataman_undo_user", "value"]
    );
    savers::sqlite(&database, &output).unwrap();
    let exported = Connection::open(output).unwrap();
    let mut statement = exported.prepare("PRAGMA table_info(input)").unwrap();
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(columns, vec!["__dataman_undo_user", "value"]);
    assert_eq!(
        exported
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
