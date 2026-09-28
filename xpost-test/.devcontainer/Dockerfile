# Aya eBPF Dev Container
# Based on: https://sookocheff.com/post/kubernetes/developing-an-aya-rs-app-using-devcontainers/
#
# Prefer plain Debian (easy arm64 on CN mirrors), then install rustup — no official rust image needed.
# Pull example:
#   docker pull --platform linux/arm64 debian:bullseye
#   # or Huawei/Aliyun mirror equivalent, then: docker tag <img> debian:bullseye
FROM debian:bullseye

USER root

# Only rustup/cargo use this proxy (do NOT name it HTTP_PROXY — Docker would inject it into every RUN).
ARG RUSTUP_PROXY=http://192.168.110.104:7890

# Apt: Aliyun, never via proxy.
RUN set -eux; \
  printf '%s\n' \
    'deb http://mirrors.aliyun.com/debian/ bullseye main contrib non-free' \
    'deb http://mirrors.aliyun.com/debian/ bullseye-updates main contrib non-free' \
    'deb http://mirrors.aliyun.com/debian-security bullseye-security main contrib non-free' \
    > /etc/apt/sources.list; \
  rm -f /etc/apt/apt.conf.d/*proxy* 2>/dev/null || true; \
  unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; \
  apt-get update; \
  apt-get install -y --no-install-recommends \
    lsb-release wget curl ca-certificates gnupg \
    software-properties-common build-essential pkg-config \
    git fish ripgrep jq openssh-client bpftool; \
  wget -q https://apt.llvm.org/llvm.sh; \
  chmod a+x llvm.sh; \
  ./llvm.sh 18; \
  apt-get install -y --no-install-recommends \
    libclang-18-dev libpolly-18-dev libzstd-dev; \
  rm -rf /var/lib/apt/lists/* llvm.sh

ENV LLVM_SYS_180_PREFIX=/usr/lib/llvm-18
ENV PATH="/usr/lib/llvm-18/bin:/usr/local/cargo/bin:${PATH}"
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo

# Install rustup + nightly (proxy only for this layer)
RUN export HTTP_PROXY="${RUSTUP_PROXY}" HTTPS_PROXY="${RUSTUP_PROXY}" \
           http_proxy="${RUSTUP_PROXY}" https_proxy="${RUSTUP_PROXY}" \
  && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --default-toolchain nightly --profile minimal --component rust-src \
  && rustup target add aarch64-unknown-linux-musl

# rustc ships libLLVM-*-rust-*.so as a GNU ld INPUT() script, not a real DSO.
# bpf-linker dlopen()s that path and fails; replace with a symlink to the real .so.
RUN set -eux; \
  libdir="$(rustc --print sysroot)/lib"; \
  for f in "$libdir"/libLLVM-*-rust-*.so; do \
    [ -e "$f" ] || continue; \
    if head -c 6 "$f" | grep -q INPUT; then \
      real="$(sed -n 's/^INPUT(\(.*\))$/\1/p' "$f")"; \
      [ -n "$real" ] && [ -e "$libdir/$real" ]; \
      rm -f "$f"; \
      ln -s "$real" "$f"; \
    fi; \
  done

# Aya tools + eza (proxy only for this layer)
RUN export HTTP_PROXY="${RUSTUP_PROXY}" HTTPS_PROXY="${RUSTUP_PROXY}" \
           http_proxy="${RUSTUP_PROXY}" https_proxy="${RUSTUP_PROXY}" \
  && cargo install bpf-linker \
  && cargo install bindgen-cli \
  && cargo install eza --locked

# fish: alias ee = eza -lh
RUN mkdir -p /root/.config/fish \
  && printf '%s\n' "alias ee 'eza -lh'" > /root/.config/fish/config.fish

WORKDIR /workspaces