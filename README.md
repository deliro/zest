# Zest

Zest is a lightweight, read-only Linux dashboard for a single machine. It runs
as one executable with the web UI embedded and works without a database or an
internet connection.

- See CPU usage, load, available memory, uptime, and CPU temperature.
- Inspect process CPU, resident memory, and disk I/O, alongside physical disk metrics.
- View interface addresses and traffic, Ethernet link speed and duplex, and Wi-Fi
  SSID, signal, and link rates when the kernel and driver expose them.
- Follow recent trends in the browser. The view refreshes every three seconds
  and history stays in memory for the lifetime of the process.

Zest targets 64-bit Linux on ARM64 (`aarch64`) and x86-64 (`x86_64`), including
servers and single-board computers such as 64-bit Orange Pi models. It reads
`/proc` and `/sys`. Process visibility, temperature, and link details vary with
kernel permissions and hardware. Disk and network views focus on device-backed
interfaces, including those exposed by virtual machine drivers. An unsupported
or inaccessible metric appears as unavailable. The example service requires
systemd 245 or newer.

## Install

Choose the archive for your `uname -m` from [GitHub Releases](https://github.com/deliro/zest/releases)
and download `SHA256SUMS` alongside it. For an ARM64 install of v0.1.0, run the
following in a clean directory. Substitute the filename of your download.

```sh
sha256sum --check --ignore-missing SHA256SUMS &&
  tar -xzf zest-v0.1.0-aarch64-unknown-linux-musl.tar.gz &&
  ./zest
```

Open <http://127.0.0.1:8080/> from that machine. `GET /healthz` returns `200`
when core telemetry is fresh and available, or `503` otherwise. For a service
that starts at boot, stop the foreground process and run these commands from
the extracted release directory on a fresh installation.

```sh
sudo install -m 0755 zest /usr/local/bin/zest
sudo install -d -m 0755 /etc/zest
sudo install -m 0644 contrib/systemd/zest.env.example /etc/zest/zest.env
sudo install -m 0644 contrib/systemd/zest.service /etc/systemd/system/zest.service
sudo systemd-analyze verify /etc/systemd/system/zest.service
sudo systemctl daemon-reload
sudo systemctl enable --now zest.service
curl -fsS --max-time 3 --retry 5 --retry-connrefused --retry-max-time 15 \
  http://127.0.0.1:8080/healthz
```

With a source build, use `target/release/zest` for the executable in the first
command. Edit `/etc/zest/zest.env` and run `sudo systemctl restart zest.service`
to apply new settings. Keep the existing environment file when upgrading. The
unit runs as a dynamic user with read capabilities for protected process metrics.
It bounds memory and task counts, requires memory locking by default, and
restarts after a process exit. It discards application logs and disables core
dumps by default. Use the foreground executable to see startup errors during
initial configuration.

## Configuration

| Environment variable | Default | Behavior |
| --- | --- | --- |
| `ZEST_BIND` | `127.0.0.1:8080` | Numeric listen address and port. Use `[::1]:8080` for IPv6 loopback. |
| `ZEST_ALLOWED_HOSTS` | empty | Up to 16 comma-separated ASCII DNS names, such as `monitor.example.test`. |
| `ZEST_LOCK_MEMORY` | `0` for the executable, `1` in the unit | At `1`, startup requires Linux memory locking and sufficient `RLIMIT_MEMLOCK`. |
| `ZEST_TEMPERATURE_PATH` | automatic | Optional CPU temperature path in the form `/sys/class/hwmon/hwmonN/tempM_input` or `/sys/class/thermal/thermal_zoneN/temp`. |

Zest validates each HTTP `Host` against the connection's destination IP or an
allowed DNS name, with `localhost` accepted on loopback. The port in `Host`
must match the listen port. For a reverse proxy on the same machine, forward
to `http://127.0.0.1:8080` and set the upstream `Host` to `127.0.0.1:8080`.

The HTTP endpoint exposes process and network details and has no built-in
authentication or TLS. Keep the default loopback binding behind an authenticated
TLS reverse proxy for access beyond the machine, or explicitly bind a trusted
interface and restrict access at the network boundary. Host validation checks the
requested host name and port.

Temperature discovery looks for recognized CPU and SoC sensors. Where discovery
cannot identify the sensor, set the override to the CPU sensor path for your
device. An override must resolve to a device under `/sys/devices`.

## Build and test

Use a current stable Rust toolchain on Linux.

```sh
cargo build --release --locked
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
```

To build a statically linked executable, use the matching musl target. The
same command can cross-build when Rustup supplies the target for your toolchain.

```sh
target=aarch64-unknown-linux-musl  # or x86_64-unknown-linux-musl
rustup target add "$target"
host_libdir=$(rustc --print target-libdir)
PATH="$host_libdir/../bin:$PATH" \
RUSTFLAGS='-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static' \
  cargo build --release --locked --target "$target"
```

To assemble the binary archive and checksum with Bash, `jq`, and `sha256sum`,
use the same toolchain that built the executable.

```sh
rustup component add rust-docs --toolchain stable
bash contrib/release/package.sh "target/$target/release/zest" "$target" "dist/$target" stable
```

Zest's own Rust crate denies `unsafe` code and uses `panic = "abort"` in release
builds. Dependencies and the Rust standard library have their own safety
boundaries. The service supervises process exits, including panics.

## License

Zest is released under the [MIT license](LICENSE). [Third-party notices](THIRD-PARTY-NOTICES.md)
cover bundled htmx and the components of the binary release.
