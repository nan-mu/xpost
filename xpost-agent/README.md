# Agent operation

Build the Agent on the Mac connected to the DUT. Its host workspace produces
one `xpost-agent` binary for both daemon and CLI commands.

```sh
cd xpost-agent
cargo build --release --locked
export PATH="$PWD/target/aarch64-apple-darwin/release:$PATH"
xpost-agent --help
```

## Run the daemon

Use `xpost-agent serve` for foreground debugging. Stop it with Ctrl+C in
its terminal. The daemon drains accepted device operations before it exits.

For background use, run the following commands from the same Worktree.

```sh
xpost-agent start
xpost-agent status
xpost-agent logs
xpost-agent dut status
xpost-agent get map rxcnt
```

`start` waits for the daemon to become ready. The daemon continues to run after
the terminal closes. A repeat `start` returns the same instance when the
configuration and executable identity match. `status` reports the loaded
executable, build revision, configuration paths, and current DUT state as JSON.
An idle daemon reports `unknown`. Use `dut status` to query the App.
`logs` prints the background service log. The daemon also keeps the rotating
`serial.log` transcript.

After a successful rebuild, use `xpost-agent restart` to load the new binary.
Use the same `--bind` and `--log-dir` options to retain custom settings.
Use `xpost-agent stop` to release the device. Wait until the experiment ends.
A busy daemon refuses `stop` and `restart`, including while work continues
after a client times out. Starting, stopping, and restarting affect only this
Worktree's managed instance.

Set `XPOST_ROOT` to override the root used for builds and default profiles.
Set `XPOST_PROFILES` to select a different profiles file.
The daemon resolves relative overrides and `--log-dir` from the invoking
directory. The default log directory is `target/xpost-agent`.
The operating system releases the USB ownership lock when the daemon exits.
Keep the lock file in place while any daemon uses it.

## Connect a client

Run `xpost-agent <command> --help` to see its arguments. Device commands print
JSON and exit nonzero on HTTP, decoding, or device errors. Local device
commands check the Worktree and the running executable identity. If the build
differs, use `status` and `logs` to inspect it before an explicit `restart`.

On the Mac, the daemon listens on `127.0.0.1:9240` by default. To accept
TR3000 requests, start with an explicit LAN bind.

```sh
xpost-agent start --bind 0.0.0.0:9240
```

Keep the existing `[agent].endpoint` on TR3000 set to the Mac's HTTP address.
Expose this unauthenticated HTTP interface only on the trusted experiment
network. For an intentional connection from another host, specify its
endpoint on the CLI.

```sh
xpost-agent --endpoint http://lab-mac:9240 dut status
```

The CLI replaces the old MCP entry point. Stop any old Agent process
explicitly before you start the updated daemon.

## Host checks

Use the normal release build for USB operation. The `test-device` Cargo
feature enables a fixture for host tests. The fixture uses a file as the DUT
identity and never opens USB when `XPOST_AGENT_TEST_DEVICE` selects it.

From this directory, build two fixture binaries at the same Git revision.

```sh
cargo build --offline --locked --features test-device --target-dir target/fixture-a
CARGO_PROFILE_DEV_DEBUG=0 cargo build --offline --locked --features test-device --target-dir target/fixture-b
python3 tests/lifecycle.py \
  target/fixture-a/aarch64-apple-darwin/debug/xpost-agent \
  target/fixture-b/aarch64-apple-darwin/debug/xpost-agent
python3 tests/cli_http.py target/aarch64-apple-darwin/release/xpost-agent
```
