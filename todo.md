# TODO
- [ ] `TryFrom<Path>` amd `TryFrom<String>` is only two inputs. Within these two we can decide which file to use, .csv, .sqlite3, .tsv.
- [ ] undo/redo. 
    - <https://www.sqlite.org/undoredo.html>
    - <https://github.com/Ocead/sqlite-undo>
- [ ] writer for writing to csv when saving
- [ ] joins
- [ ] table of tables. 
	- is this a good idea?
- [ ] rwlock for regex?
- [ ] input types, multiple or one.

from regex docs
this is better I think than arc mutex stuff I am doing onw.

- [x] make it so you can view more than the ca 50 you view when you open app. Use offset and each time we go past height increase `CurrentView` offset by height.
