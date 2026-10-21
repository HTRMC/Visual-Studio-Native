fn main() {
    // Icon resource 1: gpui uses it for the window and taskbar, Explorer for the exe.
    println!("cargo:rerun-if-changed=assets/vsn.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/vsn.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
