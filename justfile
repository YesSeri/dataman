# Define a run task with a dynamic argument for the log file
run log_file='dataman.error.log':
    mv dataman.error.log dataman.error.log.bak 2> /dev/null || true
    RUST_LOG='debug' cargo run -- assets/data.csv 2> {{log_file}}
  
tail log_file='dataman.error.log':
    tail -f dataman.error.log

# Define a build task to compile the Rust project
build:
    cargo build

release:
    cargo build --release

install: release
    cp target/release/dataman ~/.local/bin/dataman

# Define a clean task to clean the Rust project
clean:
    cargo clean
