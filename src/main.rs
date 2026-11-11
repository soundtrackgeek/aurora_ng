// Release builds run without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod covers;
mod install;
mod library;
mod search;
mod settings;
mod text_input;
mod update;

use gpui::{
    App, Application, AssetSource, Bounds, KeyBinding, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions, point, prelude::*, px, size,
};
use settings::{SavedWindow, Settings};
use std::borrow::Cow;

/// The icons are compiled into the binary.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/play.svg" => include_bytes!("../assets/icons/play.svg"),
            "icons/pause.svg" => include_bytes!("../assets/icons/pause.svg"),
            "icons/previous.svg" => include_bytes!("../assets/icons/previous.svg"),
            "icons/next.svg" => include_bytes!("../assets/icons/next.svg"),
            "icons/star.svg" => include_bytes!("../assets/icons/star.svg"),
            "icons/heart.svg" => include_bytes!("../assets/icons/heart.svg"),
            "icons/heart-outline.svg" => include_bytes!("../assets/icons/heart-outline.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

fn main() {
    if install::run() {
        return;
    }
    let settings = Settings::load();
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            text_input::bind_keys(cx);
            // Ctrl+K as in Aurora; Ctrl+F because it is what most apps use.
            cx.bind_keys([
                KeyBinding::new("ctrl-k", app::FocusSearch, None),
                KeyBinding::new("ctrl-f", app::FocusSearch, None),
            ]);
            let window_bounds = settings
                .window
                .and_then(|saved| saved_window_bounds(saved, cx))
                .unwrap_or_else(|| {
                    WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(800.)), cx))
                });
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Aurora NG".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| app::AuroraApp::new(settings, window, cx)),
            )
            .expect("could not open the Aurora window");
            cx.on_window_closed(|cx| cx.quit()).detach();
            cx.activate(true);
        });
}

/// Where the window was last time, unless that is no longer on any screen
/// (like a monitor that has been unplugged).
fn saved_window_bounds(saved: SavedWindow, cx: &App) -> Option<WindowBounds> {
    if saved.width < 200. || saved.height < 150. {
        return None;
    }
    let bounds = Bounds {
        origin: point(px(saved.x), px(saved.y)),
        size: size(px(saved.width), px(saved.height)),
    };
    let on_screen = cx
        .displays()
        .iter()
        .any(|display| display.bounds().intersects(&bounds));
    if !on_screen {
        return None;
    }
    Some(if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    })
}
