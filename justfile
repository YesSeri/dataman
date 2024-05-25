# just run
# just run 'dataman.error.log' 'assets/data.csv' 'debug'
run log_file='dataman.error.log' asset='assets/data.csv' debug_lvl='debug':
    RUST_LOG={{debug_lvl}} cargo run -- {{asset}} 2> {{log_file}}
  
tail log_file='dataman.error.log':
    tail -f {{log_file}}

build:
    cargo build

release:
    cargo build --release

install: release
    cp target/release/dataman ~/.local/bin/dataman

clean:
    cargo clean
