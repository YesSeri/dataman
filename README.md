# Dataman

`dam` is an Excel-like data wrangler in the terminal, inspired by [VisiData](https://www.visidata.org/). This application allows you to manipulate and transform data using regex, logic or SQL. It reads files into an in-memory SQLite database, which we then query with dataman as a wrapper around sqlite.

## Installation

To install and run the data manager, dataman, follow these steps:

1. **Clone the repository**:
    ```sh
    git clone https://github.com/YesSeri/dataman
    cd dataman
    cargo build --release
    # copy into your path e.g
    cp ./target/release/dataman ~/.local/bin/dam
    # or if you don't want to add to path or can't get it to work
    # ./target/release/dataman
    # cp ./target/release/dataman ./dam
    dam your-data-file.csv
    # or
    # ./dam your-data-file.csv
    ```

## Commonly Used Key Commands

The following are some of the more commonly used key commands in the TUI application:

### Table Navigation
| Key Command          | Action              |
| -------------------- | ------------------- |
| `Ctrl + Right`       | Next Table          |
| `Ctrl + Left`        | Previous Table      |
| `Right/Left/Up/Down` | Move cursor         |
| `Ctrl + c`           | Quit                |
| `Ctrl + s`           | Save                |
| `M`                  | Show Metadata Table |

### Data Transformation

Creates new column unless otherwise specified.

| Key Command | Action                   |
| ----------- | ------------------------ |
| `t`         | Regex Transform          |
| `f`         | Regex Filter             |
| `e`         | Edit a Cell              |
| `m`         | Logic Operation          |
| `s`         | Sort Column              |
| `q`         | SQL Query                |
| `f`         | Regex Filter (new table) |
| `/`         | Exact Search             |

### Table Transformation

| Key Command | Action        |
| ----------- | ------------- |
| `#`         | Text to Int   |
| `$`         | Int to Text   |
| `X`         | Delete Column |
| `D`         | Delete Table  |
| `r`         | Rename Column |
| `R`         | Rename Table  |


### Usage

1. **Simple transformation**: Use key commands to perform operations such as sorting, filtering, and running SQL queries.

2. **Input transformation**: Use the `t` key command to transform data using regex. For example, to extract the first word from a column, use the following regex: `^(\w+)`. Logic operations can also be performed using the `m` key command, e.g., `col1 + col2`, or `col1 > 20`. Combining logic operations with regex filter is powerful. `$` `f` `1`, allows you to filter for logic expressions. We go first to the column we have created from our logic operation then we convert it to text and then only keep the strings that contain the text "1". User friendlyness will improve with time.
