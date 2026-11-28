FROM rust:1.96-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin tsdb
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home tsdb && mkdir /data && chown tsdb:tsdb /data
COPY --from=build /app/target/release/tsdb /usr/local/bin/tsdb
USER tsdb
CMD ["tsdb", "--listen", "0.0.0.0:8080", "--data-dir", "/data", "--max-buffer-samples", "1000", "--durability", "written", "--query-lookback-ms", "60000"]
