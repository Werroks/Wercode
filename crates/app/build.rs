//! Puts the app icon into the Windows executable. The window, the taskbar
//! and Explorer all take it from there (the window loads resource 1).
//! macOS and Linux get their icon from the app bundle and the desktop entry,
//! which belong to packaging.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows/wercode.rc");
    println!("cargo:rerun-if-changed=../../assets/brand/wercode.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("windows/wercode.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
