// read csv file to in memory database

use std::error::Error;
use std::path::Path;
use rusqlite::Connection;

#[derive(Debug)]
struct Item {
    first: String,
    last: String,
    age: String,
}

pub fn open_connection(file: &Path) -> Result<(), Box<dyn Error>> {
    let mut csv = csv::Reader::from_path(file)?;
    let headers = csv.headers()?;
    // create table with headers as columns
    let table_name = match get_table_name(file) {
        Some(name) => name,
        None => "default_table",
    };
    let s: String = headers.iter().map(|header| format!("\n\t{} TEXT", header)).collect::<Vec<String>>().join(",");
    let create_table_query = format!("CREATE TABLE IF NOT EXISTS {} ({}\n);", table_name, s );

    println!("{}", &create_table_query);
    let conn = Connection::open_in_memory()?;

    conn.execute(&create_table_query, ())?;
    let me = Item {
        first: "Steven".to_string(),
        last: "Stevenson".to_string(),
        age: "42".to_string(),
    };
    let q = format!("INSERT INTO {} VALUES (?1, ?2, ?3)",table_name);
    dbg!(&q);
    conn.execute(
        &q,
        (&me.first, &me.last, &me.age),
    )?;
    //
    let mut stmt = conn.prepare("SELECT firstname, lastname, age FROM data")?;
    let person_iter = stmt.query_map([], |row| {
        Ok(Item {
            first: row.get(0)?,
            last: row.get(1)?,
            age: row.get(2)?,
        })
    })?;
    //
    for person in person_iter {
        println!("Found person {:?}", person.unwrap());
    }
    Ok(())
}

fn get_table_name(file: &Path) -> Option<&str> {
    file.file_stem()?.to_str()
}

// tests

#[cfg(test)]
mod tests {
    use super::*;
}
