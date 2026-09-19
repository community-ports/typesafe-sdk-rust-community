//! Captures the compiling `rustc` version so the SDK can report it in the
//! `X-TypeSafe-Runtime` header, mirroring the Python SDK's `python/<version>` runtime tag.

use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        // "rustc 1.96.0 (abcdef 2026-05-25)" -> "1.96.0"
        .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TYPESAFE_SDK_RUSTC_VERSION={version}");
}
