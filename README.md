# rust-tsdb

A time series database written in rust as an engineering thesis.

Example usage:

```sh
cargo run -- --data-dir ./data --max-buffer-samples 10000
```

## Demo: Grafana + TSDB

Requires Docker Engine and Docker Compose v2. Start from the repository root:

```sh
docker compose up --build --remove-orphans
```

Open http://localhost:3000, log in with `admin` / `demo-admin`, and open
**Dashboards → TSDB PC Telemetry**. API: http://localhost:8080. Both ports bind
only to loopback. Named volumes preserve data. Start the Windows collector below;
the dashboard starts empty and builds real history as measurements arrive.

### Live Windows PC telemetry (Docker Desktop + WSL 2)

Run the collector on **Windows**, outside the containers, so the measurements
refer to the Windows PC rather than the Docker/WSL virtual machine. No extra
packages or administrator privileges are normally needed (PowerShell 5.1+).
From the repository root in WSL, use:

```sh
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w "$PWD/demo/telemetry/windows.ps1")"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w "$PWD/demo/telemetry/windows.ps1")" -IntervalSeconds 1
```

`Bypass` applies only to this PowerShell process. Alternatively run the script in
Windows PowerShell, using its Windows path. Stop with Ctrl+C.

Options: `-IntervalSeconds 1`, `-Endpoint http://localhost:8080`,
`-ComputerLabel my-pc`, `-Once`, `-DryRun`. For example, append `-Once -DryRun`
to inspect one JSON batch without writing to the database. Windows localhost must
reach the published Docker Desktop port; test `http://localhost:8080/readyz` from
Windows if writes fail. The collector retries on the next interval.

Telemetry defaults to one-second intervals; collection and HTTP time count toward
that interval. Slow counters or requests can delay a sample. Grafana refreshes
every second with a one-second minimum query step (long time ranges may use a
larger step). Restart an already-running collector to pick up the new default.

The collector reads Windows CIM performance counters: total CPU busy percentage,
physical RAM usage, fixed-disk space, disk read/write bytes/s, and network
receive/send bytes/s per interface. Throughput is computed by Windows, so the
query engine does not need `rate()`. Adapter labels may include virtual adapters;
CPU values need not exactly match Task Manager's different utilization counter.
Missing counters are reported and skipped.
Only measurements and host/volume/interface labels are sent, not processes or files.

In Grafana Explore, select **TSDB**, use **Code** mode and try:

```promql
pc_cpu_usage_percent{source="windows"}
pc_memory_usage_percent{source="windows"}
pc_disk_usage_percent{volume="C:"}
```

For a presentation, start the collector, open a few applications or download a
file, and observe CPU/RAM/network changes. Restart the database with
`docker compose restart tsdb` to demonstrate preserved history. Let the collector
run for a few minutes before presenting. After stopping it, the last value can
remain visible for the query engine's five-minute lookback period.

`docker compose down` preserves volumes. `docker compose down -v` deletes all
stored samples and Grafana state.

## Query API

GET and form-encoded POST are supported:

```sh
curl -G http://localhost:8080/api/v1/query --data-urlencode 'query=pc_cpu_usage_percent{source="windows"}'
curl -G http://localhost:8080/api/v1/query_range --data-urlencode 'query=pc_cpu_usage_percent' --data-urlencode 'start=2026-11-16T12:00:00Z' --data-urlencode 'end=2026-11-16T12:05:00Z' --data-urlencode 'step=5s'
```

`time` defaults to now; range queries require `start`, `end`, `step`. Timestamps
accept RFC3339 or Unix seconds, including decimals. Steps accept seconds or
`ms`, `s`, `m`, `h` units (minimum 1 ms). Internally time is stored in milliseconds.
Each evaluation returns the latest sample within the preceding five minutes,
including the boundary. Output timestamps identify evaluation times, not original
sample times. Missing series are omitted; no interpolation is performed.
Limits: 11,000 evaluation times and 1,000,000 returned points per query.

Supported syntax: named selectors, equality matchers, double-quoted label values
with escapes (`\n`, `\r`, `\t`, `\\`, `\"`). An empty label value also matches
an absent label. Constants and addition of two constants support Grafana's
connection probe. Functions, regexes, range-vector selectors, vector arithmetic,
label discovery, and stale markers are not implemented. Use Code mode with metrics
lookup disabled. This is a Prometheus API subset, not a full PromQL engine.

Health endpoints: `/healthz`, `/readyz`, `/api/v1/status/buildinfo`.
Existing JSON ingestion remains at `/debug/write_json`, raw reads at `/debug/read`,
and Prometheus Remote Write at `/api/v1/write`. The demo uses `written` WAL
acknowledgements; the restart scenario demonstrates process restart persistence,
not durability against host power failure.

Validation:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

After starting Compose, verify **Save & test** on the TSDB datasource, the live PC
telemetry curves, Explore filtering, and preserved history after a server restart.
