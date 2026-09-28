# Cross-build test probe for TR3000 (aarch64-unknown-linux-musl).
FROM debian:bullseye

RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    clang \
    llvm \
    libelf-dev \
    linux-libc-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add aarch64-unknown-linux-musl

WORKDIR /work/test

COPY test/Cargo.toml test/Cargo.lock ./
COPY test/test/Cargo.toml test/
COPY test/test-common/Cargo.toml test-common/
COPY test/test-report/Cargo.toml test-report/
COPY test/test-ebpf/Cargo.toml test-ebpf/
COPY test/xtask/Cargo.toml xtask/

# Stub members for dependency cache (sources mounted at run time).
RUN mkdir -p test/src test-common/src test-report/src test-ebpf/src xtask/src \
    && echo 'fn main(){}' > test/src/main.rs \
    && echo 'pub fn lib(){}' > test-common/src/lib.rs \
    && echo 'pub fn lib(){}' > test-report/src/lib.rs \
    && echo 'pub fn lib(){}' > test-ebpf/src/lib.rs \
    && echo 'fn main(){}' > xtask/src/main.rs

RUN cargo fetch

CMD ["cargo", "build", "-p", "test", "--release", "--target", "aarch64-unknown-linux-musl"]
