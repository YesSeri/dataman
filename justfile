# just run
# just run 'dataman.error.log' 'assets/data.csv' 'debug'
run log_file='dataman.error.log' asset='assets/data.csv' debug_lvl='debug':
    RUST_LOG={{debug_lvl}} cargo run -- {{asset}} 2> {{log_file}}
  
run-release log_file='dataman.error.log' asset='assets/data.csv' debug_lvl='error':
    RUST_LOG={{debug_lvl}} cargo run --release -- {{asset}} 2> {{log_file}}

tail log_file='dataman.error.log':
    tail -f {{log_file}}

build:
    cargo build

release:
    cargo build --release

install: release
    cp target/release/dataman ~/.local/bin/dam

clean:
    cargo clean

# Pinned Python 3.10.12; project-local VisiData v3.5, never system vd.
parity-setup:
    python3 tools/reference.py

parity-verify:
    cargo build --locked
    python3 tools/parity.py verify

# Explicit maintenance operation. Review changes before committing.
parity-reference-update:
    python3 tools/parity.py reference-update

parity-report:
    python3 tools/parity.py report

parity-test:
    cargo test --locked --lib --bins
    python3 -m unittest discover -s tools -p 'test_*.py'

parity-pty:
    cargo build --locked
    python3 tools/pty_smoke.py

parity-run base:
    python3 tools/runner.py run --base {{base}} --paid

parity-resume run_id:
    python3 tools/runner.py resume --run-id {{run_id}} --paid

parity-status run_id:
    python3 tools/runner.py status --run-id {{run_id}}

parity-fake-run base:
    python3 tools/runner.py run --base {{base}} --agent tools/fake_agent.py
