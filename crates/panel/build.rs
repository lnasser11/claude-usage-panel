//! Embeds an application manifest so the settings window gets modern
//! (comctl32 v6) controls and the process is per-monitor-v2 DPI aware.
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(embed_manifest::new_manifest("Lucca.ClaudeUsagePanel"))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
