// Release builds run without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod library;

use gpui::{
    App, Application, AssetSource, Bounds, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions, prelude::*, px, size,
};
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
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Aurora NG".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| app::AuroraApp::new(window, cx)),
        )
        .expect("could not open the Aurora window");
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
