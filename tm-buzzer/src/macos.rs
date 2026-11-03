// AppKit calls raylib does not expose, sent through the Objective-C runtime; taken from whoami.
// objc_msgSend must be cast to the exact signature of each message before calling it.
use std::ffi::{CStr, c_void};
use std::mem::transmute;

type Id = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const i8) -> Id;
    fn sel_registerName(name: *const i8) -> Id;
    fn objc_msgSend();
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
}

// runs the current thread's run loop for up to `seconds`, or until it handles one source. CoreMIDI
// delivers device changes through the main run loop; without it a client keeps an old device list
pub fn run_loop(seconds: f64) {
    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 1) };
}

unsafe fn app() -> Id {
    let f: unsafe extern "C" fn(Id, Id) -> Id =
        unsafe { transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe {
        f(
            objc_getClass(c"NSApplication".as_ptr()),
            sel_registerName(c"sharedApplication".as_ptr()),
        )
    }
}

// the messages below take one word-sized argument: an NSInteger, an NSUInteger or an object pointer
unsafe fn send(receiver: Id, selector: &CStr, arg: isize) {
    let f: unsafe extern "C" fn(Id, Id, isize) =
        unsafe { transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { f(receiver, sel_registerName(selector.as_ptr()), arg) }
}

// no Dock icon and no menu bar; call after raylib init, which sets the policy to regular
pub fn set_accessory() {
    const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;
    unsafe {
        send(
            app(),
            c"setActivationPolicy:",
            NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY,
        )
    }
}

// hides the app and gives focus back to the previously active app; 0 is a nil sender
pub fn hide() {
    unsafe { send(app(), c"hide:", 0) }
}

// shows the window on the active Space; by default it stays on the Space it was created on,
// and activating the app switches to that Space
//
// # Safety
// `window` must be a live NSWindow, such as raylib's `get_window_handle` returns on macOS
pub unsafe fn move_to_active_space(window: *mut c_void) {
    const NS_WINDOW_COLLECTION_BEHAVIOR_MOVE_TO_ACTIVE_SPACE: isize = 1 << 1;
    unsafe {
        send(
            window,
            c"setCollectionBehavior:",
            NS_WINDOW_COLLECTION_BEHAVIOR_MOVE_TO_ACTIVE_SPACE,
        )
    }
}
