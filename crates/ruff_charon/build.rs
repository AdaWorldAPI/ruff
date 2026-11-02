//! Records the commit of the Charon checkout this crate is built against
//! (the path dependency `../../../charon/charon`), so `rows.bin` provenance can
//! name it. Any failure yields an empty value, which the store reads as UNKNOWN;
//! this script never fails the build.

use std::path::PathBuf;
use std::process::Command;

#[expect(
    clippy::disallowed_methods,
    reason = "a build script reads the real filesystem; the ty System abstraction does not apply"
)]
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let repo = manifest.map(|m| m.join("../../../charon"));
    let mut commit = String::new();
    if let Some(repo) = repo {
        for rel in [".git/HEAD", ".git/refs/heads"] {
            let p = repo.join(rel);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
        if let Ok(out) = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            && out.status.success()
        {
            commit = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        }
    }
    println!("cargo:rustc-env=RUFF_CHARON_CHARON_COMMIT={commit}");
}
