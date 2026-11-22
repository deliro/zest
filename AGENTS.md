# Zest: purpose and engineering goals

Zest is a minimal, read-only web dashboard for one Linux machine: an at-a-glance
alternative to opening terminal tools such as htop and iotop. It shows system
load, CPU and memory usage, temperature, processes, disk I/O, and network/link
activity. Target typical ARM64 and x86-64 servers and single-board computers,
including Orange Pi; do not promise support for every kernel or device.

The expected usage is occasional: someone may open the dashboard for ten seconds
every few days. Keep the always-running cost small, not just request latency.

## Resource requirements

- **Zero runtime disk use for application state.** Do not introduce a database,
  persisted history, log files, caches, temporary files, or background writes.
  Embed UI assets in the executable and keep bounded history in RAM. Read live
  telemetry from procfs, sysfs, and passive kernel interfaces. Binary loading and
  startup configuration reads are expected; installation/build artifacts occupy
  disk. This goal concerns Zest, not unrelated OS or workload I/O.
- Prevent application memory from reaching disk through swap in the supplied
  service. Preserve mandatory memory locking and fail startup if requested
  locking cannot be established. Keep core dumps disabled and service output
  discarded. Do not claim that systemd itself never records lifecycle events.
- **Minimize RAM and CPU by design.** Bound retained samples, device discovery,
  input sizes, connection counts, work queues, and concurrent work. Reuse collected
  snapshots across requests. Avoid subprocesses, repeated device enumeration,
  unnecessary allocations, and work that scales without a limit.
- Perform gzip compression only when a client requests a snapshot and accepts
  gzip; keep the low compression level and bounded worker path. Never compress
  every collection tick without a request. Preserve uncompressed responses for
  clients that do not accept compression.
- Prefer simple code with low steady-state cost over speculative tuning. Do not
  add an allocator, worker pool, dependency, or background task without a concrete
  need and measured benefit. Keep the existing system allocator by default.

## Architecture and behavior

- Rust, Tokio **current_thread**, Axum, and Maud. Generate HTML with Maud rather
  than handwritten HTML strings. Use embedded htmx polling and minimal JavaScript;
  no frontend framework, CDN, or runtime internet dependency.
- Preserve bounded in-memory history and the current three-second polling model
  unless the task explicitly changes them. Serve a compact dark single-page UI,
  with current values and historical graphs together. Avoid redundant labels and
  wasted header space. Check desktop layouts for unwanted scrolling.
- Keep telemetry passive and read-only: no Wi-Fi scans, network reconfiguration,
  disk probes that generate workload, or process control features.
- Treat disappearing processes, inaccessible metrics, missing sensors, and
  disconnected interfaces as normal. Show unavailable values honestly; never
  invent zero readings. Use CPU/SoC sensor identification, not fixed sensor indices.
- Keep machine-specific settings in documented `ZEST_*` environment variables.
  Never hardcode deployment addresses, hostnames, SSIDs, usernames, or local paths.
  Default to loopback; retain Host validation and existing request limits.
- Preserve the systemd service's startup/restart behavior, filesystem restrictions,
  capability bounds, and memory/task limits. Environment changes need matching
  README and example configuration updates. The service has no built-in auth/TLS.

## Code quality and verification

- Keep the strict Rust and Clippy lint policy in `Cargo.toml`; do not weaken it to
  make a change pass. Avoid panic paths, `unwrap`, `expect`, unchecked indexing,
  and unsafe code. Propagate errors, including CLI output errors, or degrade an
  optional metric gracefully. Release `panic = "abort"` is not a substitute for
  handling errors and is not proof that dependencies cannot panic.
- Preserve release `lto = "fat"`, `panic = "abort"`, and the small static binary
  packaging approach unless a requested change justifies revisiting them.
- For code changes run `cargo fmt --all --check`, `cargo test --locked`, and
  `cargo clippy --locked --all-targets -- -D warnings`. Verify Linux-only paths on
  Linux or with the appropriate target checks; a macOS build alone is insufficient.
  Use meaningful fixtures for hardware variation, races, and malformed input.
  Documentation-only edits need a diff review, not a redundant runtime test run.
- For resource-sensitive changes, measure a release build on Linux: RSS, locked
  memory, swap, threads, CPU over a stated interval, and `/proc/<pid>/io` write
  bytes. Check idle and requested-page behavior. Use passive measurements; report
  the observation window instead of claiming a finite sample proves lifetime zero I/O.
- Check UI changes in a browser, including polling and compact desktop viewports.
  For service changes verify health, enablement, errors, and restart behavior;
  never reboot a deployment machine without separate authorization.
- Keep public source, Git history, release archives, screenshots, and examples free
  of private deployment data. Use synthetic fixtures and documentation addresses.
  Include third-party license notices in binary releases. Do not silently extend
  tested hardware claims beyond actual tests.

## Repository map

- `src/collector.rs`, `metrics.rs`: bounded collection and telemetry models.
- `src/temperature.rs`, `wireless/`: hardware discovery and passive link details.
- `src/main.rs`, `config.rs`, `compression.rs`: HTTP/runtime, configuration, gzip.
- `src/render.rs`, `assets/`: Maud UI, CSS, minimal JS, embedded htmx.
- `contrib/systemd/`: generic service and environment example.
- `contrib/release/`, `.github/workflows/`: packaging, notices, and Linux CI.

Read the relevant implementation and README before editing. Keep changes focused,
and report what changed, what was actually verified, and any remaining limits.
