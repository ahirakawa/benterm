//! The page of a browser pane as WebKit shows it, in a window that is
//! never put on the screen.
//!
//! WebKit only works on the main thread, so this file has no test
//! harness: `main` runs the checks one after the other and panics at the
//! first that fails. The page is served by a small HTTP server of the
//! test's own on this machine.

use std::cell::Cell;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::FromRawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_ui::browser::{Page, PageEdit, PageEvent, page_in_view};
use block2::{Block, RcBlock};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::{Bounds, point, px, size};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

const TIMEOUT: Duration = Duration::from_secs(30);

/// The page tells the server of every key it is given.
const PAGE: &str = "<!doctype html><title>hello</title><body>page<script>\
    addEventListener('keydown', e => fetch('/key?' + e.key + (e.metaKey ? '&cmd' : '')));\
    </script></body>";

/// A page with a field that has the keyboard, which tells the server what
/// is in the field each time it changes.
const FORM: &str = "<!doctype html><title>form</title><body>\
    <input id=field autofocus oninput=\"fetch('/value?' + encodeURIComponent(this.value))\">\
    <script>field.focus()</script></body>";

/// The page a request for `/redirect` is sent on to.
const LANDED: &str = "<!doctype html><title>landed</title><body>landed</body>";

/// The page the script of the page at `/hop` sends its pane on to.
const HELD: &str = "<!doctype html><title>held</title><body>held</body>";

/// Whether the server answers a request for `/held`. Until it does, the
/// request waits.
static RELEASED: AtomicBool = AtomicBool::new(false);

/// Whether the server answers a request for `/broken`. Until it does,
/// it hangs up on one without a word.
static MENDED: AtomicBool = AtomicBool::new(false);

/// Serves [`FORM`] at `/form`, [`LANDED`] at `/landed`, [`PAGE`] at every
/// other path but `/key` and `/value`, and sends the path of each
/// request. A request for `/redirect` is sent on to `/landed` at another
/// name of this machine, still over http, and so is the page at `/hop`
/// by a script of its own, to `/held`. See [`RELEASED`] for `/held` and
/// [`MENDED`] for `/broken`.
fn serve() -> (u16, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sender, requests) = mpsc::channel();
    // Each connection is answered by itself: one that WebKit opens ahead
    // and asks nothing on, or one that is held, keeps no other waiting.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sender = sender.clone();
            std::thread::spawn(move || answer(stream, port, &sender));
        }
    });
    (port, requests)
}

/// Answers the one request of a connection, and sends its path.
fn answer(mut stream: TcpStream, port: u16, sender: &mpsc::Sender<String>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
        line.clear();
    }
    let path = first.split(' ').nth(1).unwrap_or_default().to_owned();
    if path.is_empty() || sender.send(path.clone()).is_err() {
        return;
    }
    while path == "/held" && !RELEASED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let hop = format!(
        "<!doctype html><title>hop</title><body><script>\
         location.href = 'http://localhost:{port}/held';</script></body>"
    );
    let body = if path.starts_with("/key") || path.starts_with("/value") {
        ""
    } else if path == "/form" {
        FORM
    } else if path == "/landed" {
        LANDED
    } else if path == "/held" {
        HELD
    } else if path == "/hop" {
        hop.as_str()
    } else {
        PAGE
    };
    let _ = if path == "/broken" && !MENDED.load(Ordering::SeqCst) {
        Ok(())
    } else if path == "/redirect" {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/landed\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        )
    } else {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    };
}

/// A port of this machine that refuses every connection for as long as
/// what is returned with it is kept. The port is that of a socket that
/// is connected to a listener of the test's own: no other program is
/// given the port meanwhile, and the socket takes no connection.
fn refusing_port() -> (u16, (TcpStream, TcpListener)) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let to = listener.local_addr().unwrap().port();
    let address = |port: u16| libc::sockaddr_in {
        sin_len: size_of::<libc::sockaddr_in>() as u8,
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
        },
        sin_zero: [0; 8],
    };
    let length = size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: the calls of the system with their own signatures, on a
    // socket that the stream then owns.
    let stream = unsafe {
        let socket = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        assert!(socket >= 0);
        let stream = TcpStream::from_raw_fd(socket);
        // Any port that is free, before the socket is connected.
        let (any, listening) = (address(0), address(to));
        let bound = libc::bind(socket, (&raw const any).cast(), length);
        assert_eq!(bound, 0);
        let connected = libc::connect(socket, (&raw const listening).cast(), length);
        assert_eq!(connected, 0);
        stream
    };
    (stream.local_addr().unwrap().port(), (stream, listener))
}

/// Lets the main thread's run loop, which WebKit answers on, run for a
/// moment.
fn pump() {
    unsafe {
        let run_loop: Retained<AnyObject> = msg_send![class!(NSRunLoop), currentRunLoop];
        let until: Retained<AnyObject> =
            msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 0.02f64];
        let mode = NSString::from_str("kCFRunLoopDefaultMode");
        let _: bool = msg_send![&*run_loop, runMode: &*mode, beforeDate: &*until];
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        pump();
    }
}

fn pump_for(time: Duration) {
    let until = Instant::now() + time;
    while Instant::now() < until {
        pump();
    }
}

/// How often the stand-in for the window's view was offered a key with
/// command held, and whether it takes the key, as GPUI's view does for a
/// key of the app.
static OFFERED: AtomicUsize = AtomicUsize::new(0);
static TAKES: AtomicBool = AtomicBool::new(false);

extern "C" fn perform_key_equivalent(_: &AnyObject, _: Sel, _: *mut AnyObject) -> Bool {
    OFFERED.fetch_add(1, Ordering::SeqCst);
    Bool::new(TAKES.load(Ordering::SeqCst))
}

extern "C" fn accepts_first_responder(_: &AnyObject, _: Sel) -> Bool {
    Bool::YES
}

/// A view that, as GPUI's does, answers the offer of a key itself and
/// does not pass it on to its children.
fn window_view_class() -> &'static AnyClass {
    let mut class = ClassBuilder::new(c"BentermTestWindowView", class!(NSView)).unwrap();
    unsafe {
        class.add_method(
            sel!(performKeyEquivalent:),
            perform_key_equivalent as extern "C" fn(_, _, _) -> _,
        );
        class.add_method(
            sel!(acceptsFirstResponder),
            accepts_first_responder as extern "C" fn(_, _) -> _,
        );
    }
    class.register()
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// A window that is never shown, and the view in it that stands in for
/// the one GPUI draws in.
fn hidden_window() -> (Retained<AnyObject>, Retained<AnyObject>) {
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        // No icon in the Dock, and the app never comes to the front.
        let prohibited = 2isize;
        let _: bool = msg_send![&*app, setActivationPolicy: prohibited];
        let window: Allocated<AnyObject> = msg_send![class!(NSWindow), alloc];
        let (borderless, buffered) = (0usize, 2usize);
        let window: Retained<AnyObject> = msg_send![
            window,
            initWithContentRect: rect(0., 0., 800., 600.),
            styleMask: borderless,
            backing: buffered,
            defer: true
        ];
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];
        let view: Allocated<AnyObject> = msg_send![window_view_class(), alloc];
        let view: Retained<AnyObject> = msg_send![view, initWithFrame: rect(0., 0., 800., 600.)];
        let content: Retained<AnyObject> = msg_send![&*window, contentView];
        let _: () = msg_send![&*content, addSubview: &*view];
        (window, view)
    }
}

/// Cmd-T being pressed, as AppKit hands it to a window.
fn cmd_t(window: &AnyObject) -> Retained<AnyObject> {
    unsafe {
        let (key_down, command, key_t) = (10usize, 1usize << 20, 17u16);
        let number: isize = msg_send![window, windowNumber];
        let t = NSString::from_str("t");
        msg_send![
            class!(NSEvent),
            keyEventWithType: key_down,
            location: NSPoint::new(0., 0.),
            modifierFlags: command,
            timestamp: 0f64,
            windowNumber: number,
            context: std::ptr::null::<AnyObject>(),
            characters: &*t,
            charactersIgnoringModifiers: &*t,
            isARepeat: false,
            keyCode: key_t
        ]
    }
}

/// A key with `characters` being pressed, without modifiers.
fn plain_key(window: &AnyObject, characters: &str, code: u16) -> Retained<AnyObject> {
    unsafe {
        let key_down = 10usize;
        let number: isize = msg_send![window, windowNumber];
        let text = NSString::from_str(characters);
        msg_send![
            class!(NSEvent),
            keyEventWithType: key_down,
            location: NSPoint::new(0., 0.),
            modifierFlags: 0usize,
            timestamp: 0f64,
            windowNumber: number,
            context: std::ptr::null::<AnyObject>(),
            characters: &*text,
            charactersIgnoringModifiers: &*text,
            isARepeat: false,
            keyCode: code
        ]
    }
}

/// The text on the clipboard of the system, which the checks of Cut
/// change and put back.
fn clipboard() -> Option<String> {
    unsafe {
        let board: Retained<AnyObject> = msg_send![class!(NSPasteboard), generalPasteboard];
        let kind = NSString::from_str("public.utf8-plain-text");
        let text: Option<Retained<NSString>> = msg_send![&*board, stringForType: &*kind];
        text.map(|text| text.to_string())
    }
}

fn set_clipboard(text: Option<&str>) {
    unsafe {
        let board: Retained<AnyObject> = msg_send![class!(NSPasteboard), generalPasteboard];
        let _: isize = msg_send![&*board, clearContents];
        if let Some(text) = text {
            let kind = NSString::from_str("public.utf8-plain-text");
            let text = NSString::from_str(text);
            let _: bool = msg_send![&*board, setString: &*text, forType: &*kind];
        }
    }
}

fn drain(events: &mut UnboundedReceiver<PageEvent>) -> Vec<PageEvent> {
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    seen
}

fn keyboard_is_in(window: &AnyObject, view: &AnyObject) -> bool {
    unsafe {
        let responder: Option<Retained<AnyObject>> = msg_send![window, firstResponder];
        let Some(responder) = responder else {
            return false;
        };
        let is_view: bool = msg_send![&*responder, isKindOfClass: class!(NSView)];
        is_view && msg_send![&*responder, isDescendantOf: view]
    }
}

// A stand-in for WKNavigationAction that carries a real NSURLRequest.
extern "C" fn action_request(action: &AnyObject, _: Sel) -> *mut AnyObject {
    unsafe {
        *action
            .class()
            .instance_variable(c"request")
            .unwrap()
            .load_ptr(action)
    }
}

extern "C" fn action_target_frame(action: &AnyObject, _: Sel) -> *mut AnyObject {
    unsafe {
        *action
            .class()
            .instance_variable(c"targetFrame")
            .unwrap()
            .load_ptr(action)
    }
}

extern "C" fn frame_is_main(frame: &AnyObject, _: Sel) -> Bool {
    unsafe {
        *frame
            .class()
            .instance_variable(c"main")
            .unwrap()
            .load_ptr(frame)
    }
}

static POPUP_LOADS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn record_load(_: &AnyObject, _: Sel, _: *mut AnyObject) -> *mut AnyObject {
    POPUP_LOADS.fetch_add(1, Ordering::Relaxed);
    std::ptr::null_mut()
}

extern "C" fn blob_url(_: &AnyObject, _: Sel) -> *mut AnyObject {
    let url = NSString::from_str("blob:http://localhost/test");
    unsafe { msg_send![class!(NSURL), URLWithString: &*url] }
}

fn check_navigation_policy(page: &dyn Page, web_view: &AnyObject) {
    let mut action_class = ClassBuilder::new(c"BentermTestAction", class!(NSObject)).unwrap();
    action_class.add_ivar::<*mut AnyObject>(c"request");
    action_class.add_ivar::<*mut AnyObject>(c"targetFrame");
    let mut frame_class = ClassBuilder::new(c"BentermTestFrame", class!(NSObject)).unwrap();
    frame_class.add_ivar::<Bool>(c"main");
    let original_class = web_view.class();
    let mut spy = ClassBuilder::new(c"BentermTestNavigationView", original_class).unwrap();
    let policy = sel!(webView:decidePolicyForNavigationAction:decisionHandler:);
    let mut failures = Vec::new();
    // The subclass adds no ivars. It intercepts popup loads before WebKit
    // can act on them and supplies a non-HTTP URL for the address-bar check.
    unsafe {
        action_class.add_method(sel!(request), action_request as extern "C" fn(_, _) -> _);
        action_class.add_method(
            sel!(targetFrame),
            action_target_frame as extern "C" fn(_, _) -> _,
        );
        let action_class = action_class.register();
        frame_class.add_method(sel!(isMainFrame), frame_is_main as extern "C" fn(_, _) -> _);
        let frame_class = frame_class.register();
        let frame: Retained<AnyObject> = msg_send![frame_class, new];
        let main_ivar = frame_class.instance_variable(c"main").unwrap();
        let action: Retained<AnyObject> = msg_send![action_class, new];
        let request_ivar = action_class.instance_variable(c"request").unwrap();
        let target_frame_ivar = action_class.instance_variable(c"targetFrame").unwrap();
        let target_frame = target_frame_ivar.load_ptr::<*const AnyObject>(&action);
        spy.add_method(
            sel!(loadRequest:),
            record_load as extern "C" fn(_, _, _) -> _,
        );
        spy.add_method(sel!(URL), blob_url as extern "C" fn(_, _) -> _);
        let spy = spy.register();
        let responds: Bool = msg_send![web_view, respondsToSelector: policy];
        let has_policy = responds.as_bool();
        AnyObject::set_class(web_view, spy);
        for (address, top_level_allowed, iframe_allowed) in [
            ("http://localhost/", true, true),
            ("https://localhost/", true, true),
            ("benterm-audit-test://example", false, false),
            ("file:///unused-test-file", false, false),
            ("blob:http://localhost/test", false, true),
            ("about:blank", false, true),
            ("about:srcdoc", false, true),
            ("data:text/plain,test", false, true),
        ] {
            let text = NSString::from_str(address);
            let url: Retained<AnyObject> = msg_send![class!(NSURL), URLWithString: &*text];
            let request: Retained<AnyObject> =
                msg_send![class!(NSURLRequest), requestWithURL: &*url];
            request_ivar
                .load_ptr::<*const AnyObject>(&action)
                .write(Retained::as_ptr(&request));
            for (frame_name, main, allowed) in [
                ("main frame", Some(true), top_level_allowed),
                ("iframe", Some(false), iframe_allowed),
                ("no target frame", None, top_level_allowed),
            ] {
                if let Some(main) = main {
                    main_ivar.load_ptr::<Bool>(&frame).write(Bool::new(main));
                    target_frame.write(Retained::as_ptr(&frame));
                } else {
                    target_frame.write(std::ptr::null());
                }
                let decisions = Cell::new(Vec::new());
                let handler = RcBlock::new(|value: isize| {
                    let mut values = decisions.take();
                    values.push(value);
                    decisions.set(values);
                });
                if has_policy {
                    let block: &Block<dyn Fn(isize)> = &handler;
                    let _: () = msg_send![web_view, webView: web_view,
                        decidePolicyForNavigationAction: &*action, decisionHandler: block];
                }
                let decisions = decisions.take();
                if decisions != [isize::from(allowed)] {
                    failures.push(format!(
                        "wrong navigation decision for {address} in {frame_name}: {decisions:?} (delegate responds: {has_policy})"
                    ));
                }
            }
            POPUP_LOADS.store(0, Ordering::Relaxed);
            let popup: *mut AnyObject = msg_send![web_view, webView: web_view,
                createWebViewWithConfiguration: std::ptr::null::<AnyObject>(),
                forNavigationAction: &*action, windowFeatures: std::ptr::null::<AnyObject>()];
            if !popup.is_null()
                || POPUP_LOADS.load(Ordering::Relaxed) != usize::from(top_level_allowed)
            {
                failures.push(format!("wrong popup load behavior for {address}"));
            }
        }
        if page.state().url.is_some() {
            failures.push("the address bar shows a stale URL for a blob page".to_owned());
        }
        AnyObject::set_class(web_view, original_class);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn main() {
    let (port, requests) = serve();
    let (window, window_view) = hidden_window();
    let (sender, mut events) = unbounded();
    let view = NonNull::new(Retained::as_ptr(&window_view).cast_mut().cast::<c_void>()).unwrap();
    let mut page: Box<dyn Page> =
        unsafe { page_in_view(view, sender) }.expect("this is the main thread");
    let web_view: Retained<AnyObject> = unsafe {
        let children: Retained<AnyObject> = msg_send![&*window_view, subviews];
        let count: usize = msg_send![&*children, count];
        assert_eq!(count, 1, "the page is a child of the window's view");
        msg_send![&*children, objectAtIndex: 0usize]
    };
    let hidden = || -> bool { unsafe { msg_send![&*web_view, isHidden] } };
    assert!(hidden(), "a page is hidden until it is put somewhere");

    // Deliver WebKit's delegate callback without killing a system process.
    let _: () = unsafe { msg_send![&*web_view, webViewWebContentProcessDidTerminate: &*web_view] };
    assert_eq!(drain(&mut events), [PageEvent::ProcessTerminated]);
    assert_eq!(page.state().failure, None);

    // Opening an address asks the server for the page.
    let url = format!("http://127.0.0.1:{port}/docs");
    page.load(&url);
    assert_eq!(page.state().url.as_deref(), Some(url.as_str()));
    let mut asked = Vec::new();
    wait_until("the server is asked for the page", || {
        asked.extend(requests.try_iter());
        asked.iter().any(|path| path == "/docs")
    });
    println!("ok: opening an address asks the server for the page");

    // The page tells the pane when it changes.
    wait_until("the page has its title", || {
        page.state().title.as_deref() == Some("hello") && !page.state().loading
    });
    assert!(drain(&mut events).contains(&PageEvent::Changed));
    assert_eq!(page.state().url.as_deref(), Some(url.as_str()));
    println!("ok: the page tells its title and that it changed");
    check_navigation_policy(&*page, &web_view);
    println!("ok: navigation and popup schemes are checked and a blob URL hides the old address");

    // A page that is sent on to another name of this machine, over http
    // as before, is loaded from there.
    page.load(&format!("http://127.0.0.1:{port}/redirect"));
    wait_until("the page it was sent on to is shown", || {
        page.state().title.as_deref() == Some("landed") && !page.state().loading
    });
    let landed = format!("http://localhost:{port}/landed");
    assert_eq!(page.state().url.as_deref(), Some(landed.as_str()));
    assert_eq!(page.state().failure, None);
    println!("ok: a page that is sent on to another host over http is loaded");

    // A page with nothing in it whose script sends the pane on, to a
    // server that takes its time: the page is loading for as long, and
    // has not failed.
    page.load(&format!("http://127.0.0.1:{port}/hop"));
    wait_until("the server is asked for the page sent on to", || {
        requests.try_iter().any(|path| path == "/held")
    });
    // The page hears of the load on the run loop, which may be after
    // the server hears of it.
    wait_until("the page is loading", || page.state().loading);
    assert_eq!(page.state().failure, None);
    RELEASED.store(true, Ordering::SeqCst);
    wait_until("the page sent on to is shown", || {
        page.state().title.as_deref() == Some("held") && !page.state().loading
    });
    let held = format!("http://localhost:{port}/held");
    assert_eq!(page.state().url.as_deref(), Some(held.as_str()));
    println!("ok: a page is loading while the server it was sent on to is waited for");

    // A page at a port that takes no connection says that it failed,
    // with the address and what went wrong.
    let (refusing, _kept) = refusing_port();
    let refused = format!("http://127.0.0.1:{refusing}/");
    drain(&mut events);
    page.load(&refused);
    wait_until("the page says that it failed", || {
        page.state().failure.is_some() && !page.state().loading
    });
    let failure = page.state().failure.unwrap();
    assert_eq!(failure.url, refused);
    assert!(!failure.reason.is_empty());
    assert!(drain(&mut events).contains(&PageEvent::Changed));
    println!("ok: a page that cannot be connected to says so and why");

    // So does a page whose server hangs up. Loading again asks for the
    // address that failed, and a page that is then loaded has not
    // failed.
    let broken = format!("http://127.0.0.1:{port}/broken");
    page.load(&broken);
    wait_until("the page says that it failed again", || {
        let failure = page.state().failure;
        failure.is_some_and(|failure| failure.url == broken) && !page.state().loading
    });
    MENDED.store(true, Ordering::SeqCst);
    page.reload();
    // The title comes a moment after the page is loaded.
    wait_until("the page that failed is shown", || {
        let state = page.state();
        state.title.as_deref() == Some("hello") && state.failure.is_none() && !state.loading
    });
    assert_eq!(page.state().url.as_deref(), Some(broken.as_str()));
    println!("ok: loading again asks for the address that failed");

    // A place in the window, counted from the top, is one in the view,
    // counted from the bottom.
    page.place(Some(Bounds::new(
        point(px(100.), px(50.)),
        size(px(400.), px(300.)),
    )));
    let frame: NSRect = unsafe { msg_send![&*web_view, frame] };
    assert_eq!(frame, rect(100., 250., 400., 300.));
    assert!(!hidden());
    println!("ok: the page is put where its pane is");

    // Given the keyboard by its pane, the page has it and does not say
    // so: the pane knows.
    page.set_keyboard(true);
    assert!(keyboard_is_in(&window, &web_view));
    assert!(page.has_keyboard());
    assert!(!drain(&mut events).contains(&PageEvent::TookKeyboard));
    page.set_keyboard(false);
    assert!(!page.has_keyboard());
    println!("ok: the page takes the keyboard its pane gives it");

    // Taking the keyboard itself, as AppKit makes a view that is
    // clicked do, the page says so.
    let took: bool = unsafe { msg_send![&*window, makeFirstResponder: &*web_view] };
    assert!(took && page.has_keyboard());
    assert!(drain(&mut events).contains(&PageEvent::TookKeyboard));
    println!("ok: a page that takes the keyboard itself says so");

    // A key the window's view takes is not given to the page, though the
    // page has the keyboard.
    pump_for(Duration::from_millis(300));
    asked.clear();
    asked.extend(requests.try_iter());
    asked.clear();
    let key = cmd_t(&window);
    TAKES.store(true, Ordering::SeqCst);
    let taken: bool = unsafe { msg_send![&*window, performKeyEquivalent: &*key] };
    assert!(taken);
    assert_eq!(OFFERED.load(Ordering::SeqCst), 1);
    pump_for(Duration::from_millis(700));
    asked.extend(requests.try_iter());
    assert_eq!(asked, Vec::<String>::new(), "the page was given the key");
    println!("ok: a key the window's view takes does not reach the page");

    // A key the window's view does not take is not taken by the page
    // either at this point: it goes on as a key press, which the view
    // with the keyboard gets.
    TAKES.store(false, Ordering::SeqCst);
    let taken: bool = unsafe { msg_send![&*window, performKeyEquivalent: &*key] };
    assert!(!taken);
    assert_eq!(OFFERED.load(Ordering::SeqCst), 2);
    let _: () = unsafe { msg_send![&*web_view, keyDown: &*key] };
    wait_until("the page is given the key press", || {
        asked.extend(requests.try_iter());
        asked.iter().any(|path| path == "/key?t&cmd")
    });
    println!("ok: a key the window's view leaves reaches the page as a key press");

    // The commands of the Edit menu work in a field of the page. Cut and
    // Select All are what the menu bar sends the view with the keyboard
    // for Cmd-X and Cmd-A; Undo and Redo come through the pane.
    let form = format!("http://127.0.0.1:{port}/form");
    page.load(&form);
    wait_until("the form is shown", || {
        page.state().title.as_deref() == Some("form") && !page.state().loading
    });
    let mut values = Vec::new();
    let mut wait_for_value = |what: &str, value: &str| {
        let wanted = format!("/value?{value}");
        wait_until(what, || {
            values.extend(
                requests
                    .try_iter()
                    .filter(|path| path.starts_with("/value")),
            );
            values.last() == Some(&wanted)
        });
    };
    for (character, code) in [("a", 0u16), ("b", 11), ("c", 8)] {
        let _: () =
            unsafe { msg_send![&*web_view, keyDown: &*plain_key(&window, character, code)] };
    }
    wait_for_value("abc is typed in the field", "abc");
    let kept = clipboard();
    unsafe {
        let _: () = msg_send![&*web_view, selectAll: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![&*web_view, cut: std::ptr::null::<AnyObject>()];
    }
    wait_for_value("the field is cut", "");
    let cut = clipboard();
    set_clipboard(kept.as_deref());
    assert_eq!(cut.as_deref(), Some("abc"));
    page.edit(PageEdit::Undo);
    wait_for_value("the cut is undone", "abc");
    page.edit(PageEdit::Redo);
    wait_for_value("the cut is done again", "");
    page.edit(PageEdit::Undo);
    wait_for_value("the cut is undone again", "abc");
    let kept = clipboard();
    page.edit(PageEdit::SelectAll);
    page.edit(PageEdit::Cut);
    wait_for_value("the pane cuts the field", "");
    let cut = clipboard();
    set_clipboard(kept.as_deref());
    assert_eq!(cut.as_deref(), Some("abc"));
    println!("ok: undo, redo, cut and select all work in a field of the page");

    // The pane pastes into the field and copies what is selected in it.
    let kept = clipboard();
    set_clipboard(Some("xyz"));
    page.edit(PageEdit::Paste);
    wait_for_value("the pane pastes into the field", "xyz");
    set_clipboard(None);
    page.edit(PageEdit::SelectAll);
    page.edit(PageEdit::Copy);
    // The page's own process writes the clipboard, a moment later.
    wait_until("the pane copies the field", || {
        clipboard().as_deref() == Some("xyz")
    });
    set_clipboard(kept.as_deref());
    println!("ok: copy and paste work in a field of the page");

    // Hiding the page hands the keyboard back to the window's view.
    page.place(None);
    assert!(hidden());
    assert!(!keyboard_is_in(&window, &web_view));
    let responder: Option<Retained<AnyObject>> = unsafe { msg_send![&*window, firstResponder] };
    assert_eq!(
        responder.map(|responder| Retained::as_ptr(&responder)),
        Some(Retained::as_ptr(&window_view))
    );
    println!("ok: a hidden page hands the keyboard back");

    // Closing the pane takes the page out of the window.
    drop(page);
    let children: usize = unsafe {
        let children: Retained<AnyObject> = msg_send![&*window_view, subviews];
        msg_send![&*children, count]
    };
    assert_eq!(children, 0);
    println!("ok: a closed page is taken out of the window");
}
