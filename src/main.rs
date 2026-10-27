// Release builds are GUI programs: no console window from the Start menu,
// and `vsn .` hands the terminal back at once, like `code .`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod workbench;

use std::{borrow::Cow, path::PathBuf};

use gpui_kit::component::ThemeRegistry;
use gpui_kit::*;

use workbench::Workbench;

fn main() {
    // Like `code`: folders become the workspace root, files open as editors.
    let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = std::env::args()
        .skip(1)
        .filter_map(|a| std::path::absolute(a).ok().filter(|p| p.exists()))
        .partition(|p| p.is_dir());
    // No folder given: the one open when vsn last quit, as VS Code reopens it.
    let session = workbench::session();
    let root = dirs.into_iter().last().or_else(|| {
        (session["last"].as_str())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
    });

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            cx.text_system()
                .add_fonts(vec![
                    Cow::Borrowed(include_bytes!("../assets/fonts/codicon.ttf")),
                    Cow::Borrowed(include_bytes!("../assets/fonts/seti.ttf")),
                ])
                .expect("load icon fonts");

            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(include_str!("../assets/themes/dark-2026.json"))
                .expect("load theme");
            workbench::extensions::reload();
            workbench::extensions::apply_themes(cx);

            let keys = workbench::key_bindings(cx.keyboard_mapper().as_ref());
            cx.bind_keys(keys);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
                window_min_size: Some(size(px(400.), px(270.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Visual Studio Code Native".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let wb = cx.new(|cx| Workbench::new(root, window, cx));
                window.focus(&wb.read(cx).focus.clone(), cx);
                wb.update(cx, |wb, cx| {
                    wb.restore_session(window, cx);
                    files
                        .into_iter()
                        .for_each(|f| wb.open_recent(f, window, cx))
                });
                wb
            })
            .expect("open window");
            cx.activate(true);
        });
}
