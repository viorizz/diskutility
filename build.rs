use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_EPOCH={epoch}");
    // re-stamp whenever sources change
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
