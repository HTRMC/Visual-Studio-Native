fn main() {
    // Icon resource 1: gpui uses it for the window and taskbar, Explorer for the exe.
    println!("cargo:rerun-if-changed=assets/vsn.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/vsn.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();

    // Help > About's commit and its date, as VS Code shows its own.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    let log = std::process::Command::new("git")
        .args(["log", "-1", "--format=%H %cI"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let (commit, date) = log.split_once(' ').unwrap_or(("unknown", "unknown"));
    println!("cargo:rustc-env=VSN_COMMIT={commit}");
    println!("cargo:rustc-env=VSN_COMMIT_DATE={date}");
}
