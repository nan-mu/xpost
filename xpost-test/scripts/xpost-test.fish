#!/usr/bin/env fish
# Build, upload, and run xpost-test on TR3000.
set -l xpost_test_project_dir (path resolve (status dirname)/..)
cd "$xpost_test_project_dir"; or exit $status

cargo build --release; or exit $status
scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test; or exit $status
ssh -t tr3000 './xpost-test'
