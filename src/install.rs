//! Installs the app on first run, so it shows up in the Start Menu as
//! "Aurora NG" and can be pinned.
//!
//! When the downloaded release exe (`update::ASSET_NAME`) starts, it copies
//! itself to `%LOCALAPPDATA%\Programs\Aurora NG\Aurora NG.exe`, adds a Start
//! Menu shortcut, starts the installed copy and quits. Builds with any other
//! file name, such as `cargo run`'s `aurora_ng.exe`, never install themselves.

use crate::update;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub const APP_NAME: &str = "Aurora NG";

fn installed_exe() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(
        base.join("Programs")
            .join(APP_NAME)
            .join(format!("{APP_NAME}.exe")),
    )
}

fn start_menu_shortcut() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("APPDATA")?);
    Some(
        base.join(r"Microsoft\Windows\Start Menu\Programs")
            .join(format!("{APP_NAME}.lnk")),
    )
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// Runs before any window opens. Returns true when the installed copy was
/// started and this process should quit. Problems are only logged: the app
/// then simply runs from where it is.
pub fn run() -> bool {
    let (Ok(exe), Some(installed)) = (std::env::current_exe(), installed_exe()) else {
        return false;
    };
    let is_download = exe
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(update::ASSET_NAME));

    if is_download && !same_path(&exe, &installed) {
        match install(&exe, &installed) {
            Ok(()) => match Command::new(&installed).spawn() {
                Ok(_) => return true,
                Err(error) => eprintln!("Installed, but could not start {installed:?}: {error}"),
            },
            Err(error) => eprintln!("Could not install {APP_NAME}: {error}"),
        }
    } else if same_path(&exe, &installed) {
        // Put the shortcut back if it was removed.
        if let Some(link) = start_menu_shortcut().filter(|link| !link.exists()) {
            if let Err(error) = create_shortcut(&link, &installed) {
                eprintln!("Could not add the Start Menu shortcut: {error}");
            }
        }
    }
    false
}

fn install(exe: &Path, installed: &Path) -> Result<(), String> {
    let dir = installed.parent().expect("the install path has a folder");
    std::fs::create_dir_all(dir).map_err(|error| format!("could not create {dir:?}: {error}"))?;
    // Fails if the installed copy is running; the download then runs as is.
    std::fs::copy(exe, installed).map_err(|error| format!("could not copy the app: {error}"))?;
    // The copy keeps the download's "from the internet" mark; the person
    // already chose to run it, so don't make Windows ask again.
    let mut zone = installed.as_os_str().to_owned();
    zone.push(":Zone.Identifier");
    std::fs::remove_file(zone).ok();
    if let Some(link) = start_menu_shortcut() {
        create_shortcut(&link, installed)
            .map_err(|error| format!("could not add the Start Menu shortcut: {error}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn create_shortcut(link: &Path, target: &Path) -> windows::core::Result<()> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IPersistFile,
            },
            UI::Shell::{IShellLinkW, ShellLink},
        },
        core::{HSTRING, Interface},
    };

    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(dir) = target.parent() {
                shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            shell_link
                .SetDescription(&HSTRING::from("Browse your music library and play music"))?;
            shell_link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
            shell_link
                .cast::<IPersistFile>()?
                .Save(&HSTRING::from(link.as_os_str()), true)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
fn create_shortcut(_link: &Path, _target: &Path) -> Result<(), String> {
    Ok(())
}
