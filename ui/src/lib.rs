#[cfg(target_family = "wasm")]
mod client;
pub mod model;
#[cfg(target_family = "wasm")]
mod views;
#[cfg(target_family = "wasm")]
mod workbench;

#[cfg(target_family = "wasm")]
mod browser {
    use gpui_kit::component::Theme;
    use gpui_kit::{prelude::*, *};
    use std::{borrow::Cow, cell::RefCell, rc::Rc, sync::Arc};
    use wasm_bindgen::prelude::*;

    gpui_kit::assets::icon_assets!(
        UiIcons,
        [
            Check,
            ChevronDown,
            ChevronUp,
            ChevronRight,
            ChevronLeft,
            Search,
            Close,
            X,
            Inbox,
            Loader
        ]
    );
    thread_local! {
        static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
    }

    #[wasm_bindgen]
    pub fn run() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Info);
        gpui_kit::platform::web_init();
        use gpui_kit::web::{CanvasFontFallback, WebBackendPreference, WebPlatform};
        let platform = Rc::new(WebPlatform::new_with_backend_and_font_fallback(
            false,
            WebBackendPreference::Auto,
            CanvasFontFallback::EmojiAndCjk,
        ));
        let http = Arc::new(platform.fetch_http_client());
        let app = Application::with_platform(platform)
            .with_http_client(http)
            .with_assets(UiIcons);
        APPLICATION.with(|application| {
            *application.borrow_mut() = Some(app.run_embedded(|cx| {
                gpui_kit::init(cx);
                cx.text_system()
                    .add_fonts(vec![
                        Cow::Borrowed(
                            include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf").as_slice(),
                        ),
                        Cow::Borrowed(
                            include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf").as_slice(),
                        ),
                        Cow::Borrowed(
                            include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf")
                                .as_slice(),
                        ),
                        Cow::Borrowed(
                            include_bytes!("../../assets/fonts/IBMPlexMono.ttf").as_slice(),
                        ),
                    ])
                    .expect("load bundled fonts");
                Theme::update(cx, |theme| {
                    theme.font_family = "IBM Plex Sans".into();
                    theme.mono_font_family = "IBM Plex Mono".into();
                    theme.font_size = px(14.);
                    theme.colors.primary = rgb(0x193bd1).into();
                    theme.colors.primary_foreground = rgb(0xffffff).into();
                    theme.colors.button_primary = rgb(0x193bd1).into();
                    theme.colors.button_primary_hover = rgb(0x1432b6).into();
                    theme.colors.button_primary_active = rgb(0x10298f).into();
                    theme.colors.button_primary_foreground = rgb(0xffffff).into();
                    theme.colors.ring = rgb(0x193bd1).into();
                    theme.radius = px(4.);
                    theme.shadow = false;
                });
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| crate::workbench::Workbench::new(window, cx))
                })
                .expect("open session library");
                if let Some(loading) = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.get_element_by_id("loading"))
                {
                    loading.remove();
                }
            }));
        });
        Ok(())
    }
}
