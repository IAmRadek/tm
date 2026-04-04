use std::cell::RefCell;
use std::sync::mpsc::Receiver;

use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSStatusBar, NSStatusBarButton, NSStatusItem,
};
use objc2_foundation::NSString;
use objc2_foundation::NSTimer;

struct Ivars {
    receiver: RefCell<Option<Receiver<Option<String>>>>,
    status_item: RefCell<Option<Retained<NSStatusItem>>>,
    button: RefCell<Option<Retained<NSStatusBarButton>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; we don't implement Drop.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TmMenuBarDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    impl Delegate {
        /// Called by NSTimer every 0.5 s — drains the label channel.
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&NSTimer>) {
            let ivars = self.ivars();
            let mut rx_guard = ivars.receiver.borrow_mut();
            let Some(rx) = rx_guard.as_mut() else { return };

            let mut latest: Option<Option<String>> = None;
            while let Ok(msg) = rx.try_recv() {
                latest = Some(msg);
            }

            let Some(label_opt) = latest else { return };

            let si_guard = ivars.status_item.borrow();
            let Some(si) = si_guard.as_ref() else { return };

            match label_opt {
                Some(label) => {
                    let btn_guard = ivars.button.borrow();
                    if let Some(btn) = btn_guard.as_ref() {
                        btn.setTitle(&NSString::from_str(&label));
                    }
                    si.setVisible(true);
                }
                None => {
                    si.setVisible(false);
                }
            }
        }

    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, receiver: Receiver<Option<String>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            receiver: RefCell::new(Some(receiver)),
            status_item: RefCell::new(None),
            button: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn set_status_item(&self, si: Retained<NSStatusItem>, button: Retained<NSStatusBarButton>) {
        *self.ivars().status_item.borrow_mut() = Some(si);
        *self.ivars().button.borrow_mut() = Some(button);
    }
}

pub fn run(label_rx: Receiver<Option<String>>) {
    let mtm = MainThreadMarker::new().expect("menubar::run must be called from the main thread");

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Create the status bar item, hidden until a task is active.
    let status_bar = NSStatusBar::systemStatusBar();
    let status_item: Retained<NSStatusItem> = status_bar.statusItemWithLength(-1.0);
    status_item.setVisible(false);

    let button = status_item.button(mtm).expect("NSStatusItem has no button");

    let delegate = Delegate::new(mtm, label_rx);
    delegate.set_status_item(status_item.retain(), button.retain());

    // Schedule a repeating NSTimer (0.5 s) calling tick: on the delegate.
    // SAFETY: delegate and selector are valid for the lifetime of the app.
    let _timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.5,
            &*delegate as &AnyObject,
            objc2::sel!(tick:),
            None,
            true,
        )
    };

    // Keep the status item alive for the duration of the app.
    std::mem::forget(status_item);

    app.run();
}
