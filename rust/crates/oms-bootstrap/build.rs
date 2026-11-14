#[path = "src/inventory.rs"]
mod inventory;
#[path = "src/limits.rs"]
mod limits;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../..")
        .canonicalize()
        .expect("native source root");
    let inventory = inventory::paths(&root).expect("native source inventory");
    // Watch source/data subtrees, never crate roots containing standalone builds.
    for path in &inventory.watches {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let mut output = String::from("pub const FILES: &[(&[u8], &str)] = &[\n");
    for path in inventory.files {
        let absolute = root.join(&path);
        let bytes = std::fs::read(&absolute).expect("native source file");
        writeln!(
            output,
            "(&{:?}, {:?}),",
            path.as_os_str().as_bytes(),
            format!("{:x}", Sha256::digest(bytes))
        )
        .unwrap();
    }
    output.push_str("];");
    std::fs::write(
        PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("source_inventory.rs"),
        output,
    )
    .expect("native source inventory output");
}
