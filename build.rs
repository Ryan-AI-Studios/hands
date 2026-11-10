#[path = "build_support/identity.rs"]
mod identity;

use std::path::Path;

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(
            embed_manifest::new_manifest("HelpingHands.Hands")
                .requested_execution_level(embed_manifest::manifest::ExecutionLevel::AsInvoker)
                .ui_access(cfg!(feature = "uiaccess")),
        )
        .expect("embed Windows application manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");

    // Once any `rerun-if-changed` is printed, Cargo watches only the listed paths. The set must
    // therefore cover every input of the label: sources, manifests, the ref file, packed-refs and
    // HEAD (see build_support/identity.rs).
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let id = identity::resolve(Path::new(&manifest_dir));
    for path in &id.watch {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=HANDS_BUILD_REV={}", id.rev);
    println!("cargo:rustc-env=HANDS_BUILD_DIRTY={}", id.dirty_flag());
}
