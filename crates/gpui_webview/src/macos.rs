use anyhow::{Context as _, Result};
use futures::FutureExt as _;
use gpui::{App, Bounds, Pixels, PlatformNativeView, Task, Window};
use objc2::{
    AnyThread as _, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained, runtime::ProtocolObject,
};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSView};
use objc2_core_graphics::CGMutablePath;
use objc2_foundation::{
    NSDictionary, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::{CAShapeLayer, CATransaction};
use objc2_web_kit::{
    WKFrameInfo, WKMediaCaptureType, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKPermissionDecision, WKScriptMessage, WKScriptMessageHandler,
    WKSecurityOrigin, WKUIDelegate, WKUserContentController, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::RefCell, rc::Rc};

struct ContainerState {
    visible_regions: RefCell<Vec<NSRect>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ContainerState]
    struct WebViewContainer;

    unsafe impl NSObjectProtocol for WebViewContainer {}

    impl WebViewContainer {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let parent = unsafe { self.superview() };
            let local = self.convertPoint_fromView(point, parent.as_deref());
            if !self.ivars().visible_regions.borrow().iter().any(|rect| {
                local.x >= rect.origin.x
                    && local.y >= rect.origin.y
                    && local.x < rect.origin.x + rect.size.width
                    && local.y < rect.origin.y + rect.size.height
            }) {
                None
            } else {
                // The superclass owns the returned view; msg_send retains it for the caller.
                unsafe { msg_send![super(self), hitTest: point] }
            }
        }
    }
);

struct HandlerState {
    callback: Rc<dyn Fn(String)>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = HandlerState]
    struct WebViewHandler;

    unsafe impl NSObjectProtocol for WebViewHandler {}

    unsafe impl WKScriptMessageHandler for WebViewHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn receive_message(&self, _: &WKUserContentController, message: &WKScriptMessage) {
            let body = unsafe { message.body() };
            if let Some(text) = body.downcast_ref::<NSString>() {
                (self.ivars().callback)(text.to_string());
            }
        }
    }

    unsafe impl WKNavigationDelegate for WebViewHandler {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_navigation(
            &self,
            _: &WKWebView,
            action: &WKNavigationAction,
            decision: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let url = unsafe { action.request() }
                .URL()
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());
            let allowed = url
                .as_deref()
                .is_some_and(|url| url == "about:blank" || url.starts_with("about:blank#"));
            decision.call((if allowed {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            },));
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_terminated(&self, _: &WKWebView) {
            (self.ivars().callback)(
                r#"{"kind":"error","error":"Canvas web process stopped. Reload the preview."}"#
                    .into(),
            );
        }
    }

    unsafe impl WKUIDelegate for WebViewHandler {
        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media_permission(
            &self,
            _: &WKWebView,
            _: &WKSecurityOrigin,
            _: &WKFrameInfo,
            _: WKMediaCaptureType,
            decision: &block2::DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            decision.call((WKPermissionDecision::Deny,));
        }
        #[unsafe(method(webView:requestDeviceOrientationAndMotionPermissionForOrigin:initiatedByFrame:decisionHandler:))]
        fn motion_permission(
            &self,
            _: &WKWebView,
            _: &WKSecurityOrigin,
            _: &WKFrameInfo,
            decision: &block2::DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            decision.call((WKPermissionDecision::Deny,));
        }
    }
);

pub struct WebView {
    view: Retained<WKWebView>,
    container: Retained<WebViewContainer>,
    parent: Retained<NSView>,
    handler: Retained<WebViewHandler>,
}

impl WebView {
    pub fn new(window: &Window, callback: Rc<dyn Fn(String)>) -> Result<Rc<Self>> {
        let main_thread =
            MainThreadMarker::new().context("WebView must be created on the main thread")?;
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("Native window handle unavailable: {error:?}"))?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            anyhow::bail!("WebView requires an AppKit window");
        };
        // GPUI owns this NSView and keeps it alive while the Window is borrowed.
        let parent = unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
            .context("The native window view is unavailable")?;
        let container = WebViewContainer::alloc(main_thread).set_ivars(ContainerState {
            visible_regions: RefCell::new(Vec::new()),
        });
        let container: Retained<WebViewContainer> =
            unsafe { msg_send![super(container), initWithFrame: NSRect::ZERO] };
        let handler = WebViewHandler::alloc(main_thread).set_ivars(HandlerState { callback });
        let handler: Retained<WebViewHandler> = unsafe { msg_send![super(handler), init] };
        let view = unsafe {
            let configuration = WKWebViewConfiguration::new(main_thread);
            configuration
                .setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(main_thread));
            configuration
                .userContentController()
                .addScriptMessageHandler_name(
                    ProtocolObject::from_ref(&*handler),
                    &NSString::from_str("zedCanvas"),
                );
            let view = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(main_thread),
                NSRect::ZERO,
                &configuration,
            );
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*handler)));
            view.setUIDelegate(Some(ProtocolObject::from_ref(&*handler)));
            view.setAllowsLinkPreview(false);
            view.setAllowsBackForwardNavigationGestures(false);
            view
        };
        container.setWantsLayer(true);
        container.setHidden(true);
        container.addSubview(&view);
        parent.addSubview(&container);
        Ok(Rc::new(Self {
            view,
            container,
            parent,
            handler,
        }))
    }

    pub fn load_html(&self, html: &str) -> Result<()> {
        unsafe {
            self.view
                .loadHTMLString_baseURL(&NSString::from_str(html), None)
        }
        .context("WebKit could not start the Canvas navigation")?;
        Ok(())
    }

    pub fn blur(&self) {
        if let Some(window) = self.view.window()
            && let Some(responder) = window.firstResponder()
            && let Some(view) = responder.downcast_ref::<NSView>()
            && view.isDescendantOf(&self.view)
            && !window.makeFirstResponder(Some(&self.parent))
        {
            log::debug!("Canvas input declined to release focus");
        }
    }

    pub fn snapshot(&self, cx: &App) -> Task<Result<Vec<u8>>> {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let sender = RefCell::new(Some(sender));
        let completion = block2::RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let result = (|| {
                if let Some(error) = unsafe { error.as_ref() } {
                    anyhow::bail!("{}", error.localizedDescription());
                }
                let image = unsafe { image.as_ref() }.context("WebKit returned no snapshot")?;
                let tiff = image
                    .TIFFRepresentation()
                    .context("Could not read Canvas snapshot")?;
                let bitmap = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
                    .context("Could not decode Canvas snapshot")?;
                let png = unsafe {
                    bitmap.representationUsingType_properties(
                        NSBitmapImageFileType::PNG,
                        &NSDictionary::new(),
                    )
                }
                .context("Could not encode Canvas snapshot")?;
                Ok(png.to_vec())
            })();
            if let Some(sender) = sender.borrow_mut().take()
                && sender.send(result).is_err()
            {
                log::debug!("Canvas snapshot request was cancelled");
            }
        });
        unsafe {
            self.view
                .takeSnapshotWithConfiguration_completionHandler(None, &completion);
        }
        let timeout = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(30));
        cx.spawn(async move |_| {
            futures::select! {
                result = receiver.fuse() => result.context("Canvas snapshot callback was dropped")?,
                _ = timeout.fuse() => anyhow::bail!(
                    "Canvas snapshot timed out; element details are still available"
                ),
            }
        })
    }

    pub fn evaluate(&self, script: &str) {
        let callback = self.handler.ivars().callback.clone();
        let completion = block2::RcBlock::new(
            move |_: *mut objc2::runtime::AnyObject, error: *mut NSError| {
                if let Some(error) = unsafe { error.as_ref() } {
                    // Runtime errors are delivered over the same channel as page diagnostics.
                    let message = error.localizedDescription().to_string();
                    let report = serde_json::json!({
                        "kind": "error",
                        "error": format!("WebKit evaluation failed: {message}")
                    });
                    callback(report.to_string());
                }
            },
        );
        unsafe {
            self.view.evaluateJavaScript_completionHandler(
                &NSString::from_str(script),
                Some(&completion),
            );
        }
    }
}

impl PlatformNativeView for WebView {
    fn set_frame(&self, bounds: Bounds<Pixels>, visible_regions: &[Bounds<Pixels>]) {
        if visible_regions.is_empty() {
            self.hide();
            return;
        }
        let width = f64::from(f32::from(bounds.size.width));
        let height = f64::from(f32::from(bounds.size.height));
        let origin_y = f64::from(f32::from(bounds.origin.y));
        let frame = NSRect::new(
            NSPoint::new(
                f64::from(f32::from(bounds.origin.x)),
                if self.parent.isFlipped() {
                    origin_y
                } else {
                    self.parent.bounds().size.height - origin_y - height
                },
            ),
            NSSize::new(width, height),
        );
        let local_regions: Vec<_> = visible_regions
            .iter()
            .map(|region| {
                NSRect::new(
                    NSPoint::new(
                        f64::from(f32::from(region.origin.x - bounds.origin.x)),
                        f64::from(f32::from(region.origin.y - bounds.origin.y)),
                    ),
                    NSSize::new(
                        f64::from(f32::from(region.size.width)),
                        f64::from(f32::from(region.size.height)),
                    ),
                )
            })
            .collect();
        *self.container.ivars().visible_regions.borrow_mut() = local_regions.clone();
        self.container.setFrame(frame);
        self.view.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
        if let Some(layer) = self.container.layer() {
            let path = CGMutablePath::new();
            for rect in local_regions {
                unsafe { CGMutablePath::add_rect(Some(&path), std::ptr::null(), rect) };
            }
            let mask = CAShapeLayer::new();
            mask.setFrame(NSRect::new(NSPoint::ZERO, frame.size));
            mask.setPath(Some(&path));
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            unsafe {
                layer.setMask(Some(&mask));
            }
            CATransaction::commit();
        }
        self.container.setHidden(false);
    }

    fn hide(&self) {
        self.blur();
        self.container.setHidden(true);
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        unsafe {
            self.view.stopLoading();
            self.view.setNavigationDelegate(None);
            self.view.setUIDelegate(None);
            self.view
                .configuration()
                .userContentController()
                .removeScriptMessageHandlerForName(&NSString::from_str("zedCanvas"));
        }
        self.container.removeFromSuperview();
    }
}
