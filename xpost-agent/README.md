# Agent operation

Run the service on the Mac connected to the DUT. Start it from `xpost-agent/`.

```sh
cargo run --release --locked -- serve
```

Keep this execution session open during the experiment. After the experiment,
press Ctrl+C. The service also accepts SIGTERM. It finishes accepted device
operations before it releases the device lock.

The service runs espflash Board Info once at startup. It restores App mode
before it opens HTTP. Read the board information in the terminal. A failed
check exits with an error. Later health requests use the startup result.
Use `dut status` to query the App.

The default bind is `127.0.0.1:9240`. For TR3000 access, use this command on the Mac.

```sh
cargo run --release --locked -- serve --bind 0.0.0.0:9240
```

Expose this unauthenticated interface only to the trusted experiment network.
Set `[agent].endpoint` on TR3000 to the Mac's HTTP address.

## Client commands

Use a second terminal in the same Worktree.

```sh
cargo run --release --locked -- --help
cargo run --release --locked -- status
cargo run --release --locked -- dut status
cargo run --release --locked -- get map rxcnt
```

Device commands print JSON. Check their exit code. By default, the CLI checks
the Worktree identity at `127.0.0.1:9240`. Set `--endpoint` to select another
HTTP origin explicitly. `status` reports `busy` while an accepted operation
runs or waits. An HTTP timeout can leave the operation running.

Use `preflight`, `preflight --inventory`, and `preflight --xdp NAME` for
offline checks. Use `preflight --remote` or `preflight --remote --xdp NAME`
to use the service's configuration. Use command help for deployment bindings,
Perf epochs and chunks, and EXB2 batch inputs.
The runner uses `GET /v1/preflight/medium/hal-aot` or `GET /v1/preflight/medium/jit-rbpf`.
The runner attests the installed deployment when it resumes a campaign.

Set `XPOST_ROOT` to override the build root. Set `XPOST_PROFILES` to choose
a profiles file. Relative overrides and `--log-dir` use the invoking directory.
The default log directory is `target/xpost-agent`. The service writes
its logs to the terminal and rotates the `serial.log` transcript.

## Host checks

The `test-device` feature replaces USB with a fixture. Use a normal release
build for USB.

```sh
cargo build --locked --features test-device
python3 tests/serve_http.py target/aarch64-apple-darwin/debug/xpost-agent
cargo build --release --locked
python3 tests/cli_http.py target/aarch64-apple-darwin/release/xpost-agent
```
