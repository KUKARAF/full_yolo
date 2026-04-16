use std::process::Command;

fn main() {
    // Rerun if HEAD changes (local dev) or if GIT_SHA env changes (CI)
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-env-changed=GIT_SHA");

    // CI sets GIT_SHA; fall back to running git locally
    let sha = std::env::var("GIT_SHA").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_string())
    });

    // FULL_YOLO_VERSION = MAJOR.MINOR (from Cargo.toml) + .shortsha
    // MAJOR.MINOR is bumped manually; shortsha is updated on every build.
    let major = env!("CARGO_PKG_VERSION_MAJOR");
    let minor = env!("CARGO_PKG_VERSION_MINOR");
    println!("cargo:rustc-env=FULL_YOLO_VERSION={major}.{minor}.{sha}");
}
