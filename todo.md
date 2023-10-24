# TODO
- [x] Add sql query `q` shortcut 
- [ ] `TryFrom<Path>` amd `TryFrom<String>` is only two inputs. Within these two we can decide which file to use, .csv, .sqlite3, .tsv.
- [x] add wrapper to `TableState` to keep track of rowid. The problem is that you can't access the data in *Ratatui's* TableState.
	- This was unneccesary because we already have rows in update function in tui. It is very inefficent because it is a new call to the db for every action, even if you just move cursor, but it is okay for now. Later the current 200 rows we are viewing should be stored in the `Database struct` and updated only when neccessary.
- [ ] Regex is compiled to much. It should be compiled once and then used. The problem is that the custom functions we give to out sqlite3 database can't recieve a compiled regex, only a string and therefore needs to be compiled every time. In the regex documentation it says that this should be avoided. Maybe a closure can solve this together with `RefCell<Rc<Regex>`. The idea is that our `Database struct` has the compiled regex and the custom functions recieve a reference to this regex. Then we can mutate the regex when we get new user input and the custom functions will use the new regex.
	- easier to just use closure inside of custom funciton adder, and not using `RefCell<Rc<Regex>>` at all, and `Database struct`. 
    - Since `rusqlite` is multithreaded I need to use `Arc<Mutex<Regex>>`.
- [ ] undo/redo. 
    - <https://www.sqlite.org/undoredo.html>
    - <https://github.com/Ocead/sqlite-undo>
- [ ] writer for writing to csv when saving
- [ ] joins
- [ ] table of tables. 
- [ ] histogram

from regex docs
```rust
use {
    once_cell::sync::Lazy,
    regex::Regex,
};

fn some_helper_function(haystack: &str) -> bool {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"...").unwrap());
    RE.is_match(haystack)
}

fn main() {
    assert!(some_helper_function("abc"));
    assert!(!some_helper_function("ac"));
}

```
this is better I think than arc mutex stuff I am doing onw.

- [x] make it so you can view more than the ca 50 you view when you open app. Use offset and each time we go past height increase `CurrentView` offset by height.
# regex 

## filter stats
- 10_000 Elapsed time: 6.33s without cache
- 10_000 Elapsed time: 74.01ms with cache

## transform stats

- 10_000 Elapsed time: 3.03s without cache, filter is cached
- 10_000 Elapsed time: 101.73ms with cache, filter is cached

## input
make it possible to input from either small input box in bottom or open new alternate window with `$EDITOR`
