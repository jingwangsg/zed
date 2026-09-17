#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use gpui::{
        Bounds, MouseButton, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
    };
    use gpui_webview::WebView;
    use std::rc::Rc;
    env_logger::init();

    struct Preview {
        webview: Rc<WebView>,
        overlay: bool,
        visible: bool,
    }

    impl Render for Preview {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(rgb(0xffffff))
                .text_color(rgb(0x222222))
                .font_family("Helvetica")
                .text_size(px(16.))
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .p_4()
                        .child(
                            div()
                                .id("toggle-overlay")
                                .cursor_pointer()
                                .child("Toggle overlay")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.overlay = !this.overlay;
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("toggle-view")
                                .cursor_pointer()
                                .child("Toggle page")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.visible = !this.visible;
                                        cx.notify();
                                    }),
                                ),
                        ),
                )
                .child(div().flex_1().min_h_0().when(self.visible, |this| {
                    this.child(gpui::native_view(self.webview.clone()).size_full())
                }))
                .when(self.overlay, |this| {
                    this.child(
                        div()
                            .id("overlay")
                            .absolute()
                            .top(px(120.))
                            .left(px(160.))
                            .w(px(300.))
                            .h(px(180.))
                            .bg(rgb(0xe8edf5))
                            .border_2()
                            .border_color(rgb(0x3267dd))
                            .occlude()
                            .p_4()
                            .child("GPUI overlay — the page must not cover this region"),
                    )
                })
        }
    }

    gpui_platform::application().run(|cx| {
        cx.on_window_closed(|cx, _| { if cx.windows().is_empty() { cx.quit(); } }).detach();
        cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(900.), px(640.)), cx))),
            ..Default::default()
        }, |window, cx| {
            let webview = WebView::new(window, Rc::new(|message| println!("WEBVIEW {message}"))).expect("create native WebView");
            webview.load_html(r#"<!doctype html><html><body style="font:18px -apple-system;padding:28px;background:#fff;color:#222"><h1>Native Canvas input test</h1><p>Type or paste Chinese text, copy it, and switch focus to GPUI controls.</p><input aria-label="Canvas input" style="font:inherit;width:500px" oninput="webkit.messageHandlers.zedCanvas.postMessage(JSON.stringify({input:this.value}))"><p><button onclick="this.textContent='Clicked';webkit.messageHandlers.zedCanvas.postMessage('button clicked')">Click page button</button></p><div style="height:800px;background:linear-gradient(#d7e3ff,#ffe6cb)">Scrollable native content</div></body></html>"#).expect("load native page");
            cx.new(|_| Preview { webview, overlay: false, visible: true })
        }).expect("open native preview window");
        cx.activate(true);
    });
}
