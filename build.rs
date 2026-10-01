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
}
