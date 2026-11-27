fn main() {
    let workloads = [
        "NOOP",
        "PRIME",
        "LOG2",
        "COPY",
        "FLETCHER32",
        "BSORT",
        "STRCMP",
        "MATMUL",
    ];
    let enabled = workloads
        .iter()
        .filter(|name| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some())
        .count();
    assert_eq!(enabled, 1, "select exactly one EXB2 HAL workload feature");
    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");
}
