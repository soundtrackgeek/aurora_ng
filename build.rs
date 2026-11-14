//! Embeds the app icon and version info into the Windows exe, and lists the
//! country flags so they can be compiled in.

use std::{env, fs, path::Path};

fn main() {
    flags();
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

/// Writes `flags.rs`: every `assets/flags/xx.svg`, sorted by country code.
fn flags() {
    println!("cargo:rerun-if-changed=assets/flags");
    let root = env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut codes: Vec<String> = fs::read_dir(Path::new(&root).join("assets/flags"))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().ok()?;
            name.strip_suffix(".svg").map(str::to_owned)
        })
        .collect();
    codes.sort();
    let entries: String = codes
        .iter()
        .map(|code| {
            format!(
                "    (\"{code}\", include_bytes!(r\"{root}/assets/flags/{code}.svg\")),
"
            )
        })
        .collect();
    let source = format!(
        "static FLAGS: &[(&str, &[u8])] = &[
{entries}];
"
    );
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("flags.rs"),
        source,
    )
    .unwrap();
}
