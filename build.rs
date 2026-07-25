//! Embed a Windows application manifest so the loader binds Common Controls v6 (comctl32 v6).
//! Without it, native-windows-gui's static comctl32 imports fail to resolve at load
//! (STATUS_ENTRYPOINT_NOT_FOUND, 0xC0000139). The default manifest also enables visual styles
//! and per-monitor DPI awareness.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("T10sKeepAwake"))
            .expect("unable to embed application manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
