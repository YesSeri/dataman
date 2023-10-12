use rusqlite::{Connection, Row};
use rusqlite::ffi::sqlite3changegroup_output;
use rusqlite::functions::FunctionFlags;
use crate::error::AppResult;

pub fn scalar_function_example(db: Connection) -> rusqlite::Result<()> {
    println!("scalar_function_example");

    let mut stmt = db.prepare("SELECT * FROM data WHERE lastname REGEXP 'k'")?;
    let iter = stmt.query_map([], |row | {
        // let result = row.get::<_, i32>(0)?;
        let mut results: Vec<String> = vec![];
        let mut i = 1;
        while let Ok(a) = row.get(i) {
            println!("a: {:?}", a);
            results.push(a);
            i += 1;
        }

        Ok(results)
    })?;
    for el in iter {
        println!("{:?}", el);
    }
    // get all rows
    // let rows = db.
    todo!();
}