# rust-tsdb

A time series database written in rust as an engineering thesis.

Example usage:

```sh
cargo run -- --data-dir ./data --max-buffer-samples 10000
```

## Demo: Grafana + TSDB

Requires Docker Engine and Docker Compose v2. Start from the repository root:

```sh
docker compose up --build
```

Open http://localhost:3000, log in with `admin` / `demo-admin`, and open
**Dashboards → TSDB PC Telemetry**. API: http://localhost:8080. Both ports bind
only to loopback. Named volumes preserve data. Start the Windows collector below;
the dashboard starts empty and builds real history as measurements arrive.

To shut down containers:

```sh
docker compose down
docker compose down -v  # delete all stored samples and Grafana state
```

### Live Windows PC telemetry (Docker Desktop + WSL 2)

Run the collector on **Windows**, outside the containers, so the measurements
refer to the Windows PC rather than the Docker/WSL virtual machine. No extra
packages or administrator privileges are normally needed (PowerShell 5.1+).
From the repository root in WSL, use:

```sh
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w "$PWD/demo/telemetry/windows.ps1")"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w "$PWD/demo/telemetry/windows.ps1")" -IntervalSeconds 1
```

Options: `-IntervalSeconds 1`, `-Endpoint http://localhost:8080`,
`-ComputerLabel my-pc`, `-Once`, `-DryRun`. For example, append `-Once -DryRun`
to inspect one JSON batch without writing to the database. Windows localhost must
reach the published Docker Desktop port; test `http://localhost:8080/readyz` from
Windows if writes fail. The collector retries on the next interval.

The collector reads Windows CIM performance counters: total CPU busy percentage,
physical RAM usage, fixed-disk space, disk read/write bytes/s, and network
receive/send bytes/s per interface. 

## Grafana Queries

In Grafana Explore, select **TSDB**, use **Code** mode and try:

```promql
pc_cpu_usage_percent{source="windows"}
pc_memory_usage_percent{source="windows"}
pc_disk_usage_percent{volume="C:"}
```

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

### Test coverage

To generate an HTML coverage report for unit and integration tests, install the
LLVM tools and `cargo-llvm-cov` once, then run the coverage command from the
repository root (in WSL):

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked
cargo llvm-cov --all-features --html
```

Open `target/llvm-cov/html/index.html` in a browser to see coverage by file and
highlighted source lines. Run `cargo llvm-cov --all-features` for a terminal summary.

