use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    for path in [
        "src",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "../profiles.toml",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for git_path in ["HEAD", "index", "packed-refs"] {
        watch_git_path(git_path);
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&branch);
    }
    if let Some(revision) = git(&["rev-parse", "HEAD"]) {
        let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
            .filter(|status| !status.is_empty())
            .map_or("", |_| "-dirty");
        println!("cargo:rustc-env=XPOST_BUILD_REVISION={revision}{dirty}");
    }
}

fn watch_git_path(name: &str) {
    if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", name])
        && std::path::Path::new(&path).exists()
    {
        println!("cargo:rerun-if-changed={path}");
    }
}
