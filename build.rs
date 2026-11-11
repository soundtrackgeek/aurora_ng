//! Embeds the app icon and version info into the Windows exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon/aurora_ng.rc");
    println!("cargo:rerun-if-changed=assets/app-icon/aurora_ng.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let env = |name| std::env::var(name).unwrap();
    let macros = [
        format!("VERSION_MAJOR={}", env("CARGO_PKG_VERSION_MAJOR")),
        format!("VERSION_MINOR={}", env("CARGO_PKG_VERSION_MINOR")),
        format!("VERSION_PATCH={}", env("CARGO_PKG_VERSION_PATCH")),
        format!("VERSION_STRING=\"{}\"", env("CARGO_PKG_VERSION")),
    ];
    embed_resource::compile("assets/app-icon/aurora_ng.rc", &macros)
        .manifest_optional()
        .unwrap();
}
