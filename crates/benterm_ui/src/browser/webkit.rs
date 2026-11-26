//! The page of a browser pane as WebKit shows it: a `WKWebView` that is a
//! child of the view the window draws in.
//!
//! The web view is a child of the window's view and not a view beside
//! it, for the keys. AppKit offers a key with command held to the views
//! of the window from the top down before the key goes to the view that
//! has the keyboard. The window's view takes the offer, looks the key up
//! among the app's keys, and does not pass the offer on to its children.
//! So a key of the app never reaches the page, and a key that is none
//! goes on to the page as a key press. A web view beside the window's
//! view would be offered the key itself, and one that has the keyboard
//! hands every key it is offered to its page.
//!
//! Everything here runs on the main thread, which the window's own
//! drawing does too.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_void};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;
use std::time::Instant;

use block2::Block;
use futures::channel::mpsc::UnboundedSender;
use gpui::{Bounds, Pixels, Window};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{MainThreadMarker, class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{LoadPhase, Page, PageEdit, PageEvent, PageFailure, PageState};

#[link(name = "WebKit", kind = "framework")]
unsafe extern "C" {}

/// Where a web view keeps what it shares with its page.
const EVENTS_IVAR: &CStr = c"bentermEvents";

/// A load as WebKit names it: a `WKNavigation`, which is the same object
/// where the load is asked for and where the view hears of it.
struct Navigation(Retained<AnyObject>);

impl PartialEq for Navigation {
    fn eq(&self, other: &Self) -> bool {
        Retained::as_ptr(&self.0) == Retained::as_ptr(&other.0)
    }
}

/// The load WebKit passed a method of the view.
///
/// # Safety
///
/// `navigation` is null or points at an object that is alive.
unsafe fn navigation(navigation: *mut AnyObject) -> Option<Navigation> {
    // SAFETY: the caller says so.
    unsafe { Retained::retain(navigation) }.map(Navigation)
}

/// What a web view shares with its page: the methods WebKit calls on the
/// view write here, and the page reads it.
struct Shared {
    /// Where the pane is told of what happens to the page.
    events: UnboundedSender<PageEvent>,
    /// Why the page asked for last is not shown.
    failure: RefCell<Option<PageFailure>>,
    latest: RefCell<Latest<Navigation>>,
    has_committed: Cell<bool>,
    /// Whether the pane is moving the keyboard itself, which it is not
    /// told of.
    moving_keyboard: Cell<bool>,
}

/// Which load of a page is the latest, so that what is heard of an
/// earlier one is not taken for news of it. `N` names a load.
struct Latest<N> {
    load: Option<N>,
    /// Whether the pane asked for `load` and it has not begun yet.
    awaited: bool,
}

impl<N: PartialEq> Latest<N> {
    fn new() -> Self {
        Self {
            load: None,
            awaited: false,
        }
    }

    /// The pane asked for a page, which made `load`, or made none.
    fn asked(&mut self, load: Option<N>) {
        if load.is_some() {
            self.load = load;
            self.awaited = true;
        }
    }

    /// `load` began. Whether it is the latest.
    fn began(&mut self, load: Option<N>) -> bool {
        if self.awaited && load.is_some() && load != self.load {
            return false;
        }
        self.awaited = false;
        if load.is_some() {
            self.load = load;
        }
        true
    }

    /// Whether `load` is the latest navigation. A load without a name is
    /// taken to be.
    fn matches(&self, load: &Option<N>) -> bool {
        load.is_none() || self.load.is_none() || *load == self.load
    }

    /// `load` failed. Whether it is the latest. A load without a name
    /// is taken to be.
    fn failed(&mut self, load: &Option<N>) -> bool {
        let latest = self.matches(load);
        if latest {
            // It is over, begun or not.
            self.awaited = false;
        }
        latest
    }
}

/// The loads that end without the page having failed: one that another
/// load took the place of, and one that became a download.
const CANCELLED: (&str, isize) = ("NSURLErrorDomain", -999);
const INTERRUPTED: (&str, isize) = ("WebKitErrorDomain", 102);

/// Where an error of a load keeps the address that failed, as an
/// `NSURL`.
const FAILING_URL: &str = "NSErrorFailingURLKey";

/// Where it keeps the same address as an `NSString`.
const FAILING_URL_TEXT: &str = "NSErrorFailingURLStringKey";

/// What of a web view its pane is told of when it changes.
const OBSERVED: [&str; 5] = ["URL", "title", "canGoBack", "canGoForward", "loading"];

/// Tells the changes this file asked to be told of from any other.
static OBSERVING: u8 = 0;

fn observing() -> *mut c_void {
    ptr::from_ref(&OBSERVING).cast_mut().cast()
}

/// `WKWebView` with what a pane needs to know: when the view takes the
/// keyboard, and when its page changes.
fn web_view_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        let mut class = ClassBuilder::new(c"BentermWebView", class!(WKWebView))
            .expect("no other class is called BentermWebView");
        class.add_ivar::<*mut c_void>(EVENTS_IVAR);
        // SAFETY: each function has the arguments and the return type of
        // the method it is added as.
        unsafe {
            class.add_method(
                sel!(becomeFirstResponder),
                become_first_responder as extern "C" fn(_, _) -> _,
            );
            class.add_method(
                sel!(observeValueForKeyPath:ofObject:change:context:),
                observe_value as extern "C" fn(_, _, _, _, _, _),
            );
            class.add_method(
                sel!(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:),
                create_web_view as extern "C" fn(_, _, _, _, _, _) -> _,
            );
            class.add_method(
                sel!(webView:decidePolicyForNavigationAction:decisionHandler:),
                decide_navigation as extern "C" fn(_, _, _, _, _),
            );
            class.add_method(
                sel!(webView:didStartProvisionalNavigation:),
                did_start as extern "C" fn(_, _, _, _),
            );
            class.add_method(
                sel!(webView:didCommitNavigation:),
                did_commit as extern "C" fn(_, _, _, _),
            );
            class.add_method(
                sel!(webView:didFinishNavigation:),
                did_finish as extern "C" fn(_, _, _, _),
            );
            class.add_method(
                sel!(webView:didFailProvisionalNavigation:withError:),
                did_fail as extern "C" fn(_, _, _, _, _),
            );
            class.add_method(
                sel!(webView:didFailNavigation:withError:),
                did_fail as extern "C" fn(_, _, _, _, _),
            );
            class.add_method(
                sel!(webViewWebContentProcessDidTerminate:),
                process_terminated as extern "C" fn(_, _, _),
            );
        }
        class.register()
    })
}

extern "C" fn process_terminated(this: &AnyObject, _: Sel, _web_view: *mut AnyObject) {
    if let Some(shared) = shared_of(this) {
        let _ = shared.events.unbounded_send(PageEvent::ProcessTerminated);
    }
}

/// Where the ivar of a web view is.
fn events_of(view: &AnyObject) -> *mut *mut c_void {
    let ivar = view
        .class()
        .instance_variable(EVENTS_IVAR)
        .expect("a BentermWebView has the ivar");
    // SAFETY: the ivar was added with this type.
    unsafe { ivar.load_ptr::<*mut c_void>(view) }
}

/// What `view` shares with its page, or none while the page does not
/// want to hear of the view.
fn shared_of(view: &AnyObject) -> Option<&Shared> {
    // SAFETY: the ivar is null or points at what its page owns, which
    // the page takes out of the ivar before it drops it.
    unsafe { (*events_of(view) as *const Shared).as_ref() }
}

fn tell(view: &AnyObject, event: PageEvent) {
    if let Some(shared) = shared_of(view) {
        // The pane is gone: nobody is left to tell.
        let _ = shared.events.unbounded_send(event);
    }
}

/// A load of the page itself began: what failed before is over.
extern "C" fn did_start(this: &AnyObject, _: Sel, _web_view: *mut AnyObject, load: *mut AnyObject) {
    let Some(shared) = shared_of(this) else {
        return;
    };
    // SAFETY: WebKit passes a WKNavigation or nothing.
    let load = unsafe { navigation(load) };
    let latest = shared.latest.borrow_mut().began(load);
    if !latest {
        return;
    }
    tell(
        this,
        PageEvent::LoadPhase {
            phase: LoadPhase::Started,
            at: Instant::now(),
        },
    );
    let failed = shared.failure.borrow_mut().take();
    if failed.is_some() {
        tell(this, PageEvent::Changed);
    }
}

/// The page received the first data of the response and began drawing it.
extern "C" fn did_commit(
    this: &AnyObject,
    _: Sel,
    _web_view: *mut AnyObject,
    load: *mut AnyObject,
) {
    let Some(shared) = shared_of(this) else {
        return;
    };
    shared.has_committed.set(true);
    // SAFETY: WebKit passes a WKNavigation or nothing.
    let load = unsafe { navigation(load) };
    if !shared.latest.borrow().matches(&load) {
        return;
    }
    tell(
        this,
        PageEvent::LoadPhase {
            phase: LoadPhase::Committed,
            at: Instant::now(),
        },
    );
}

/// The page finished loading.
extern "C" fn did_finish(
    this: &AnyObject,
    _: Sel,
    _web_view: *mut AnyObject,
    load: *mut AnyObject,
) {
    let Some(shared) = shared_of(this) else {
        return;
    };
    // SAFETY: WebKit passes a WKNavigation or nothing.
    let load = unsafe { navigation(load) };
    if !shared.latest.borrow().matches(&load) {
        return;
    }
    tell(
        this,
        PageEvent::LoadPhase {
            phase: LoadPhase::Finished,
            at: Instant::now(),
        },
    );
}

/// A load of the page itself failed, before the page was shown or
/// after.
extern "C" fn did_fail(
    this: &AnyObject,
    _: Sel,
    _web_view: *mut AnyObject,
    load: *mut AnyObject,
    error: *mut AnyObject,
) {
    let Some(shared) = shared_of(this) else {
        return;
    };
    // SAFETY: WebKit passes a WKNavigation or nothing.
    let load = unsafe { navigation(load) };
    let latest = shared.latest.borrow_mut().failed(&load);
    if !latest {
        return;
    }
    // SAFETY: WebKit passes an NSError, read with the signatures of
    // Foundation.
    let failure = unsafe {
        let Some(error) = error.as_ref() else {
            return;
        };
        let domain: Retained<NSString> = msg_send![error, domain];
        let code: isize = msg_send![error, code];
        let ended = (domain.to_string(), code);
        if [CANCELLED, INTERRUPTED].contains(&(ended.0.as_str(), ended.1)) {
            return;
        }
        let reason: Retained<NSString> = msg_send![error, localizedDescription];
        let info: Option<Retained<AnyObject>> = msg_send![error, userInfo];
        let entry = |key: &str| -> Option<Retained<AnyObject>> {
            let key = NSString::from_str(key);
            let info = info.as_ref()?;
            msg_send![&**info, objectForKey: &*key]
        };
        let url: Option<Retained<NSString>> = match (entry(FAILING_URL), entry(FAILING_URL_TEXT)) {
            (Some(url), _) if msg_send![&*url, isKindOfClass: class!(NSURL)] => {
                msg_send![&*url, absoluteString]
            }
            (_, Some(text)) if msg_send![&*text, isKindOfClass: class!(NSString)] => {
                Some(Retained::cast_unchecked(text))
            }
            _ => None,
        };
        PageFailure {
            url: url.map(|url| url.to_string()).unwrap_or_default(),
            reason: reason.to_string(),
        }
    };
    shared.failure.replace(Some(failure));
    tell(this, PageEvent::Changed);
    tell(
        this,
        PageEvent::LoadPhase {
            phase: LoadPhase::Failed,
            at: Instant::now(),
        },
    );
}

extern "C" fn become_first_responder(this: &AnyObject, _: Sel) -> Bool {
    // SAFETY: the method of the superclass, with its own signature.
    let became: Bool = unsafe { msg_send![super(this, class!(WKWebView)), becomeFirstResponder] };
    let moved = shared_of(this).is_some_and(|shared| shared.moving_keyboard.get());
    if became.as_bool() && !moved {
        tell(this, PageEvent::TookKeyboard);
    }
    became
}

extern "C" fn observe_value(
    this: &AnyObject,
    _: Sel,
    key_path: *mut AnyObject,
    object: *mut AnyObject,
    change: *mut AnyObject,
    context: *mut c_void,
) {
    if context == observing() {
        tell(this, PageEvent::Changed);
        return;
    }
    // SAFETY: a change this file did not ask for is the superclass's.
    unsafe {
        let _: () = msg_send![
            super(this, class!(WKWebView)),
            observeValueForKeyPath: key_path,
            ofObject: object,
            change: change,
            context: context
        ];
    }
}

/// Only subframes may also navigate to local document schemes.
fn allows_navigation(request: &AnyObject, is_subframe: bool) -> bool {
    // SAFETY: the request is a WebKit-provided NSURLRequest.
    unsafe {
        let url: Option<Retained<AnyObject>> = msg_send![request, URL];
        let Some(url) = url else {
            return false;
        };
        let scheme: Option<Retained<NSString>> = msg_send![&*url, scheme];
        scheme.is_some_and(|scheme| {
            let scheme = scheme.to_string();
            scheme.eq_ignore_ascii_case("http")
                || scheme.eq_ignore_ascii_case("https")
                || (is_subframe
                    && ["about", "blob", "data"]
                        .iter()
                        .any(|allowed| scheme.eq_ignore_ascii_case(allowed)))
        })
    }
}

extern "C" fn decide_navigation(
    _: &AnyObject,
    _: Sel,
    _web_view: *mut AnyObject,
    action: &AnyObject,
    handler: &Block<dyn Fn(isize)>,
) {
    // SAFETY: WebKit passes a WKNavigationAction, whose targetFrame is
    // a WKFrameInfo when present. A new window has no target frame.
    let allowed = unsafe {
        let target: Option<Retained<AnyObject>> = msg_send![action, targetFrame];
        let is_subframe = target.is_some_and(|frame| {
            let is_main: Bool = msg_send![&*frame, isMainFrame];
            !is_main.as_bool()
        });
        let request: Option<Retained<AnyObject>> = msg_send![action, request];
        request
            .as_deref()
            .is_some_and(|request| allows_navigation(request, is_subframe))
    };
    // WKNavigationActionPolicyCancel = 0, WKNavigationActionPolicyAllow = 1.
    handler.call((isize::from(allowed),));
}

/// A page that asks for a window of its own, as a link that opens in a
/// new tab does, is shown in the pane instead: a pane has one page.
extern "C" fn create_web_view(
    this: &AnyObject,
    _: Sel,
    _web_view: *mut AnyObject,
    _configuration: *mut AnyObject,
    action: *mut AnyObject,
    _features: *mut AnyObject,
) -> *mut AnyObject {
    // SAFETY: WebKit passes a WKNavigationAction, whose request the view
    // can load.
    unsafe {
        let request: Option<Retained<AnyObject>> = msg_send![action, request];
        if let Some(request) = request.filter(|request| allows_navigation(request, false)) {
            let load: Option<Retained<AnyObject>> = msg_send![this, loadRequest: &*request];
            if let Some(shared) = shared_of(this) {
                let nav = load.map(Navigation);
                let requested = nav.is_some();
                shared.latest.borrow_mut().asked(nav);
                if requested {
                    let _ = shared.events.unbounded_send(PageEvent::LoadPhase {
                        phase: LoadPhase::Requested,
                        at: Instant::now(),
                    });
                }
            }
        }
    }
    ptr::null_mut()
}

/// Where a view puts a child that the window has at `place`, given the
/// view's own `bounds` and whether its y axis points down. The window's
/// coordinates start at the top left; a view that is not flipped counts
/// from the bottom. Both are in points, so the scale of the screen does
/// not come into it.
fn frame_in(bounds: NSRect, flipped: bool, place: Bounds<Pixels>) -> NSRect {
    let (x, y) = (f64::from(place.origin.x), f64::from(place.origin.y));
    let (width, height) = (f64::from(place.size.width), f64::from(place.size.height));
    let y = match flipped {
        true => y,
        false => bounds.size.height - y - height,
    };
    NSRect::new(
        NSPoint::new(bounds.origin.x + x, bounds.origin.y + y),
        NSSize::new(width, height),
    )
}

/// The initial frame for a new web view: the pane's page area if known, or
/// 0×0 until a frame lays it out.
fn initial_frame_in(
    bounds: NSRect,
    flipped: bool,
    initial_place: Option<Bounds<Pixels>>,
) -> NSRect {
    initial_place
        .map(|place| frame_in(bounds, flipped, place))
        .unwrap_or_else(|| NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.)))
}

struct WebKitPage {
    view: Retained<AnyObject>,
    /// The view of the window, which the web view is a child of.
    parent: Retained<AnyObject>,
    /// What the web view's ivar points at.
    shared: *mut Shared,
    /// The address asked for last, which the view only has once it has
    /// started on it.
    requested: Option<String>,
}

impl WebKitPage {
    fn new(
        parent: Retained<AnyObject>,
        events: UnboundedSender<PageEvent>,
        initial_place: Option<Bounds<Pixels>>,
        _: MainThreadMarker,
    ) -> Self {
        let shared = Box::into_raw(Box::new(Shared {
            events,
            failure: RefCell::new(None),
            latest: RefCell::new(Latest::new()),
            has_committed: Cell::new(false),
            moving_keyboard: Cell::new(false),
        }));
        // SAFETY: on the main thread, with the signatures of AppKit and
        // WebKit. The ivar is set before anything can call the methods
        // that read it.
        let view = unsafe {
            let configuration: Retained<AnyObject> = msg_send![class!(WKWebViewConfiguration), new];
            let view: Allocated<AnyObject> = msg_send![web_view_class(), alloc];
            let frame = initial_place
                .map(|place| {
                    let bounds: NSRect = msg_send![&*parent, bounds];
                    let flipped: bool = msg_send![&*parent, isFlipped];
                    initial_frame_in(bounds, flipped, Some(place))
                })
                .unwrap_or_else(|| initial_frame_in(NSRect::default(), false, None));
            let view: Retained<AnyObject> =
                msg_send![view, initWithFrame: frame, configuration: &*configuration];
            events_of(&view).write(shared.cast());
            // Hidden until a frame says where the pane is.
            let _: () = msg_send![&*view, setHidden: true];
            let _: () = msg_send![&*view, setUIDelegate: &*view];
            let _: () = msg_send![&*view, setNavigationDelegate: &*view];
            for key in OBSERVED {
                let key = NSString::from_str(key);
                let _: () = msg_send![
                    &*view,
                    addObserver: &*view,
                    forKeyPath: &*key,
                    options: 0usize,
                    context: observing()
                ];
            }
            let _: () = msg_send![&*parent, addSubview: &*view];
            view
        };
        Self {
            view,
            parent,
            shared,
            requested: None,
        }
    }

    /// Moves the keyboard to `view`. The web view does not tell its
    /// pane of it: the pane asked for it, and only hears of what the
    /// user did.
    fn give_keyboard_to(&self, view: &AnyObject) {
        // SAFETY: on the main thread, with the signatures of AppKit. The
        // page owns what the pointer points at until it is dropped.
        unsafe {
            let window: Option<Retained<AnyObject>> = msg_send![&*self.view, window];
            if let Some(window) = window {
                // Only the keyboard is not told of: a load that begins
                // or fails meanwhile is.
                (*self.shared).moving_keyboard.set(true);
                let _: bool = msg_send![&*window, makeFirstResponder: view];
                (*self.shared).moving_keyboard.set(false);
            }
        }
    }
}

impl WebKitPage {
    /// Notes the load that asking the view for a page made.
    fn asked(&self, load: Option<Retained<AnyObject>>) {
        // SAFETY: the page owns what the pointer points at until it is
        // dropped.
        let shared = unsafe { &*self.shared };
        let nav = load.map(Navigation);
        let requested = nav.is_some();
        shared.latest.borrow_mut().asked(nav);
        if requested {
            let _ = shared.events.unbounded_send(PageEvent::LoadPhase {
                phase: LoadPhase::Requested,
                at: Instant::now(),
            });
        }
    }

    fn failure(&self) -> Option<PageFailure> {
        // SAFETY: the page owns what the pointer points at until it is
        // dropped.
        unsafe { (*self.shared).failure.borrow().clone() }
    }
}

impl Page for WebKitPage {
    fn load(&mut self, url: &str) {
        self.requested = Some(url.to_owned());
        // SAFETY: on the main thread, with the signatures of Foundation
        // and WebKit.
        unsafe {
            let text = NSString::from_str(url);
            let url: Option<Retained<AnyObject>> = msg_send![class!(NSURL), URLWithString: &*text];
            let Some(url) = url else {
                return;
            };
            let request: Retained<AnyObject> =
                msg_send![class!(NSURLRequest), requestWithURL: &*url];
            self.asked(msg_send![&*self.view, loadRequest: &*request]);
        }
    }

    fn go_back(&mut self) {
        // SAFETY: on the main thread, with the signature of WebKit.
        self.asked(unsafe { msg_send![&*self.view, goBack] });
    }

    fn go_forward(&mut self) {
        // SAFETY: as for going back.
        self.asked(unsafe { msg_send![&*self.view, goForward] });
    }

    /// A page that failed is asked for again at its address: the view
    /// may have no page to load again, or the one it showed before.
    fn reload(&mut self) {
        if let Some(failure) = self.failure() {
            // An error that names no address is of the page the pane
            // asked for last, as far as the page can tell.
            let named = Some(failure.url).filter(|url| super::is_page_url(url));
            if let Some(url) = named.or_else(|| self.requested.clone()) {
                return self.load(&url);
            }
        }
        // SAFETY: as for going back.
        self.asked(unsafe { msg_send![&*self.view, reload] });
    }

    fn state(&self) -> PageState {
        // SAFETY: on the main thread, with the signatures of Foundation
        // and WebKit.
        let (url, title, can_go_back, can_go_forward, loading) = unsafe {
            let url: Option<Retained<AnyObject>> = msg_send![&*self.view, URL];
            let url: Option<Retained<NSString>> = match url {
                Some(url) => msg_send![&*url, absoluteString],
                None => None,
            };
            let title: Option<Retained<NSString>> = msg_send![&*self.view, title];
            let title = title.map(|title| title.to_string());
            let back: bool = msg_send![&*self.view, canGoBack];
            let forward: bool = msg_send![&*self.view, canGoForward];
            let loading: bool = msg_send![&*self.view, isLoading];
            (
                url.map(|url| url.to_string()),
                title,
                back,
                forward,
                loading,
            )
        };
        PageState {
            url: match url {
                Some(url) => super::is_page_url(&url).then_some(url),
                // SAFETY: the page owns the shared state.
                None if !unsafe { (*self.shared).has_committed.get() } => self.requested.clone(),
                None => None,
            },
            title: title.filter(|title| !title.is_empty()),
            can_go_back,
            can_go_forward,
            loading,
            failure: self.failure(),
        }
    }

    fn place(&mut self, place: Option<Bounds<Pixels>>) {
        let Some(place) = place else {
            // A view that is hidden with the keyboard in it would keep
            // what is typed from the window.
            if self.has_keyboard() {
                self.give_keyboard_to(&self.parent);
            }
            // SAFETY: on the main thread, with the signature of AppKit.
            let _: () = unsafe { msg_send![&*self.view, setHidden: true] };
            return;
        };
        // SAFETY: on the main thread, with the signatures of AppKit.
        unsafe {
            let bounds: NSRect = msg_send![&*self.parent, bounds];
            let flipped: bool = msg_send![&*self.parent, isFlipped];
            let frame = frame_in(bounds, flipped, place);
            let _: () = msg_send![&*self.view, setFrame: frame];
            let _: () = msg_send![&*self.view, setHidden: false];
        }
    }

    /// Whether the keyboard is in the web view: in it or in a view
    /// inside it.
    fn has_keyboard(&self) -> bool {
        // SAFETY: on the main thread, with the signatures of AppKit.
        unsafe {
            let window: Option<Retained<AnyObject>> = msg_send![&*self.view, window];
            let Some(window) = window else {
                return false;
            };
            let responder: Option<Retained<AnyObject>> = msg_send![&*window, firstResponder];
            let Some(responder) = responder else {
                return false;
            };
            let is_view: bool = msg_send![&*responder, isKindOfClass: class!(NSView)];
            is_view && msg_send![&*responder, isDescendantOf: &*self.view]
        }
    }

    fn set_keyboard(&mut self, on: bool) {
        match (on, self.has_keyboard()) {
            (true, false) => self.give_keyboard_to(&self.view),
            (false, true) => self.give_keyboard_to(&self.parent),
            _ => {}
        }
    }

    /// Cut, Copy, Paste and Select All are the web view's own commands. What is
    /// undone is kept by the undo manager the web view answers with,
    /// which is the one AppKit's Undo would use with the web view
    /// having the keyboard.
    fn edit(&mut self, edit: PageEdit) {
        let nothing = ptr::null::<AnyObject>();
        // SAFETY: on the main thread, with the signatures of AppKit.
        unsafe {
            match edit {
                PageEdit::Cut => {
                    let _: () = msg_send![&*self.view, cut: nothing];
                }
                PageEdit::Copy => {
                    let _: () = msg_send![&*self.view, copy: nothing];
                }
                PageEdit::Paste => {
                    let _: () = msg_send![&*self.view, paste: nothing];
                }
                PageEdit::SelectAll => {
                    let _: () = msg_send![&*self.view, selectAll: nothing];
                }
                PageEdit::Undo | PageEdit::Redo => {
                    let undo: Option<Retained<AnyObject>> = msg_send![&*self.view, undoManager];
                    let Some(undo) = undo else {
                        return;
                    };
                    match edit {
                        PageEdit::Undo if msg_send![&*undo, canUndo] => {
                            let _: () = msg_send![&*undo, undo];
                        }
                        PageEdit::Redo if msg_send![&*undo, canRedo] => {
                            let _: () = msg_send![&*undo, redo];
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

impl Drop for WebKitPage {
    fn drop(&mut self) {
        if self.has_keyboard() {
            self.give_keyboard_to(&self.parent);
        }
        // SAFETY: on the main thread, with the signatures of AppKit and
        // WebKit. Once the ivar is null nothing reads what the view
        // shared with the page, which is then dropped.
        unsafe {
            events_of(&self.view).write(ptr::null_mut());
            for key in OBSERVED {
                let key = NSString::from_str(key);
                let _: () = msg_send![
                    &*self.view,
                    removeObserver: &*self.view,
                    forKeyPath: &*key,
                    context: observing()
                ];
            }
            let _: () = msg_send![&*self.view, setUIDelegate: ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.view, setNavigationDelegate: ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.view, stopLoading];
            let _: () = msg_send![&*self.view, removeFromSuperview];
            drop(Box::from_raw(self.shared));
        }
    }
}

/// A page in `window`, or none if the window has no view to put one in:
/// a window of a test, or one that is drawn without a screen.
pub(super) fn page_in(
    window: &Window,
    events: UnboundedSender<PageEvent>,
    initial_place: Option<Bounds<Pixels>>,
) -> Option<Box<dyn Page>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: the handle of a window that is open holds its NSView.
    unsafe { page_in_view_with_place(handle.ns_view, events, initial_place) }
}

/// A page that is a child of the `NSView` at `view`, hidden until it is
/// put somewhere. None on any thread but the main one.
///
/// # Safety
///
/// `view` points at an `NSView` that is alive.
pub unsafe fn page_in_view(
    view: NonNull<c_void>,
    events: UnboundedSender<PageEvent>,
) -> Option<Box<dyn Page>> {
    // SAFETY: the caller says it is an NSView that is alive.
    unsafe { page_in_view_with_place(view, events, None) }
}

unsafe fn page_in_view_with_place(
    view: NonNull<c_void>,
    events: UnboundedSender<PageEvent>,
    initial_place: Option<Bounds<Pixels>>,
) -> Option<Box<dyn Page>> {
    let main_thread = MainThreadMarker::new()?;
    // SAFETY: the caller says it is an NSView that is alive.
    let parent = unsafe { Retained::retain(view.as_ptr().cast::<AnyObject>()) }?;
    Some(Box::new(WebKitPage::new(
        parent,
        events,
        initial_place,
        main_thread,
    )))
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    /// The window counts from the top, a view that is not flipped from
    /// the bottom.
    #[test]
    fn a_place_in_the_window_is_counted_from_the_bottom_of_the_view() {
        let window = rect(0., 0., 1000., 700.);
        let place = Bounds::new(point(px(240.), px(60.)), size(px(500.), px(300.)));
        assert_eq!(frame_in(window, false, place), rect(240., 340., 500., 300.));
        // At the bottom of the window is at the bottom of the view.
        let low = Bounds::new(point(px(0.), px(600.)), size(px(1000.), px(100.)));
        assert_eq!(frame_in(window, false, low), rect(0., 0., 1000., 100.));
    }

    /// A page sends its pane on by itself: no load was asked for.
    #[test]
    fn a_load_the_page_began_itself_is_the_latest() {
        let mut latest = Latest::new();
        assert!(latest.began(Some(1)));
        assert!(latest.failed(&Some(1)));
        assert!(latest.began(Some(2)));
        assert!(latest.failed(&Some(2)));
        assert!(!latest.failed(&Some(1)));
    }

    /// The first load fails for a reason of its own just as the pane
    /// asks for another page: its failure is not the new page's.
    #[test]
    fn what_an_earlier_load_did_is_not_taken_for_the_one_asked_for_since() {
        let mut latest = Latest::new();
        latest.asked(Some(1));
        assert!(latest.began(Some(1)));
        latest.asked(Some(2));
        assert!(!latest.failed(&Some(1)));
        // Nor does the first load beginning late make it the latest.
        assert!(!latest.began(Some(1)));
        assert!(!latest.failed(&Some(1)));
        assert!(latest.began(Some(2)));
        assert!(latest.failed(&Some(2)));
    }

    /// A load that was asked for and fails before it begins is over: a
    /// load the page begins afterwards is the latest.
    #[test]
    fn a_load_that_failed_before_it_began_is_not_waited_for() {
        let mut latest = Latest::new();
        latest.asked(Some(1));
        assert!(latest.failed(&Some(1)));
        assert!(latest.began(Some(2)));
        assert!(latest.failed(&Some(2)));
    }

    #[test]
    fn asking_for_what_makes_no_load_leaves_the_latest() {
        let mut latest = Latest::new();
        latest.asked(Some(1));
        latest.asked(None);
        assert!(latest.failed(&Some(1)));
        assert!(latest.began(Some(1)));
    }

    #[test]
    fn a_load_without_a_name_is_taken_to_be_the_latest() {
        let mut latest = Latest::new();
        assert!(latest.failed(&None));
        latest.asked(Some(1));
        assert!(latest.failed(&None));
        assert!(latest.began(None));
        assert!(latest.failed(&Some(1)));
    }

    #[test]
    fn a_flipped_view_counts_as_the_window_does() {
        let window = rect(0., 0., 1000., 700.);
        let place = Bounds::new(point(px(240.), px(60.)), size(px(500.), px(300.)));
        assert_eq!(frame_in(window, true, place), rect(240., 60., 500., 300.));
    }

    #[test]
    fn the_origin_of_the_view_is_added() {
        let window = rect(10., 20., 1000., 700.);
        let place = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.)));
        assert_eq!(frame_in(window, false, place), rect(10., 620., 100., 100.));
    }

    #[test]
    fn initial_frame_uses_pane_place_or_falls_back_to_zero() {
        let window = rect(0., 0., 1000., 700.);
        let place = Bounds::new(point(px(240.), px(60.)), size(px(500.), px(300.)));
        assert_eq!(
            initial_frame_in(window, false, Some(place)),
            rect(240., 340., 500., 300.)
        );
        assert_eq!(initial_frame_in(window, false, None), rect(0., 0., 0., 0.));
    }

    #[test]
    fn matches_ignores_stale_navigations() {
        let mut latest = Latest::new();
        assert!(latest.matches(&Some(1)));
        latest.asked(Some(1));
        assert!(latest.matches(&Some(1)));
        assert!(!latest.matches(&Some(2)));
        assert!(latest.matches(&None));
        assert!(latest.began(Some(1)));
        latest.asked(Some(2));
        assert!(!latest.matches(&Some(1)));
        assert!(latest.matches(&Some(2)));
    }
}
