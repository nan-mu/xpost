# Cross-build xpost-test for TR3000 (aarch64-unknown-linux-musl).
FROM debian:bullseye

RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    clang \
    llvm \
    libelf-dev \
    linux-libc-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add aarch64-unknown-linux-musl

WORKDIR /work/xpost-test

COPY Cargo.toml Cargo.lock ./
COPY core/Cargo.toml core/
COPY ebpf/Cargo.toml ebpf/

# Stub members for dependency cache (sources mounted at run time).
RUN mkdir -p core/src ebpf/src \
    && echo '#![cfg_attr(not(feature = "host"), no_std)] pub const X: u8 = 0;' > core/src/lib.rs \
    && echo 'fn main(){}' > core/src/main.rs \
    && echo '#![no_std] pub fn lib(){}' > ebpf/src/lib.rs \
    && echo '#![no_std] #![no_main] #[panic_handler] fn panic(_: &::core::panic::PanicInfo) -> ! { loop {} }' > ebpf/src/main.rs

RUN cargo fetch

CMD ["cargo", "build", "-p", "xpost_test", "--release", "--target", "aarch64-unknown-linux-musl"]
