//! System notifications for threads that need a look while out of sight: a
//! turn that finished or failed, a permission to give, a question to answer.
//! Clicking one brings up its thread. Each thread holds at most one: a newer
//! one replaces it, and it is taken down once the thread is seen.
//!
//! macOS only lets an app bundle post them, so a bare `cargo run` binary
//! posts nothing.

use futures::channel::mpsc::UnboundedReceiver;

/// Start posting, asking the user's leave the first time. Gives the threads
/// whose notification was clicked, or `None` when this build can't post.
pub fn init() -> Option<UnboundedReceiver<usize>> {
    #[cfg(all(target_os = "macos", not(test)))]
    return mac::init();
    #[cfg(any(not(target_os = "macos"), test))]
    None
}

/// Post `body` under `title` for `thread`, replacing any it already has.
pub fn post(thread: usize, title: &str, subtitle: &str, body: &str) {
    #[cfg(all(target_os = "macos", not(test)))]
    mac::post(thread, title, subtitle, body);
    #[cfg(any(not(target_os = "macos"), test))]
    let _ = (thread, title, subtitle, body);
}

/// Take down `thread`'s notification, once it has been seen.
pub fn withdraw(thread: usize) {
    #[cfg(all(target_os = "macos", not(test)))]
    mac::withdraw(thread);
    #[cfg(any(not(target_os = "macos"), test))]
    let _ = thread;
}

/// A notification's text for a reply: its first lines, without markdown's
/// marks, cut to what a banner shows.
pub fn excerpt(text: &str) -> String {
    const MAX: usize = 180;
    let mut out = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("```")) {
        let line = line.trim_start_matches(['#', '>', '-', '*', ' ']);
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&line.replace("**", "").replace('`', ""));
        if out.chars().count() > MAX {
            break;
        }
    }
    if out.chars().count() > MAX {
        out = out.chars().take(MAX - 1).collect::<String>().trim_end().to_string() + "…";
    }
    out
}

fn identifier(thread: usize) -> String {
    format!("thread-{thread}")
}

#[cfg(all(target_os = "macos", not(test)))]
#[allow(unexpected_cfgs)] // objc's macros test cfgs of their own.
mod mac {
    use block::{Block, ConcreteBlock};
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
    use objc::declare::ClassDecl;
    use objc::runtime::{BOOL, Class, Object, Protocol, Sel};
    use objc::{msg_send, sel, sel_impl};
    use std::sync::{Mutex, OnceLock};

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    /// Where clicks go; set once posting is possible.
    static CLICKS: OnceLock<Mutex<UnboundedSender<usize>>> = OnceLock::new();

    // UNAuthorizationOptions and UNNotificationPresentationOptions.
    const BADGE: usize = 1 << 0;
    const SOUND: usize = 1 << 1;
    const ALERT: usize = 1 << 2;
    const LIST: usize = 1 << 3;
    const BANNER: usize = 1 << 4;

    fn center() -> Option<id> {
        CLICKS.get()?;
        let class = Class::get("UNUserNotificationCenter")?;
        // SAFETY: only reached once `init` found the app bundle the center needs.
        let center: id = unsafe { msg_send![class, currentNotificationCenter] };
        (center != nil).then_some(center)
    }

    pub fn init() -> Option<UnboundedReceiver<usize>> {
        if CLICKS.get().is_some() {
            return None;
        }
        // SAFETY: plain Foundation and UserNotifications calls on the main
        // thread; the delegate is leaked, since the center only holds it weakly.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            // Without a bundle the center throws, and there'd be no app to name.
            let bundle: id = msg_send![objc::class!(NSBundle), mainBundle];
            let name: id = msg_send![bundle, bundleIdentifier];
            let center_class = Class::get("UNUserNotificationCenter");
            if name == nil || center_class.is_none() {
                pool.drain();
                return None;
            }
            let (tx, rx) = unbounded();
            let _ = CLICKS.set(Mutex::new(tx));
            let center: id = msg_send![center_class.unwrap(), currentNotificationCenter];
            let delegate: id = msg_send![delegate_class(), new];
            let _: () = msg_send![center, setDelegate: delegate];
            let asked = ConcreteBlock::new(|_granted: BOOL, _error: id| {}).copy();
            let _: () = msg_send![center, requestAuthorizationWithOptions: ALERT | SOUND | BADGE completionHandler: &*asked];
            pool.drain();
            Some(rx)
        }
    }

    fn delegate_class() -> &'static Class {
        const NAME: &str = "ChatNotificationDelegate";
        if let Some(class) = Class::get(NAME) {
            return class;
        }
        let mut decl = ClassDecl::new(NAME, objc::class!(NSObject)).expect("notification delegate class");
        if let Some(protocol) = Protocol::get("UNUserNotificationCenterDelegate") {
            decl.add_protocol(protocol);
        }
        // SAFETY: the signatures match the delegate methods they stand for.
        unsafe {
            decl.add_method(
                sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                will_present as extern "C" fn(&Object, Sel, id, id, id),
            );
            decl.add_method(
                sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                did_receive as extern "C" fn(&Object, Sel, id, id, id),
            );
        }
        decl.register()
    }

    /// Chat in front still shows them: one is only posted for a thread out of sight.
    extern "C" fn will_present(_: &Object, _: Sel, _center: id, _notification: id, handler: id) {
        // SAFETY: the system passes a `void (^)(UNNotificationPresentationOptions)`.
        unsafe { (*(handler as *mut Block<(usize,), ()>)).call((BANNER | LIST | SOUND,)) };
    }

    extern "C" fn did_receive(_: &Object, _: Sel, _center: id, response: id, handler: id) {
        // SAFETY: a UNNotificationResponse, and a `void (^)(void)` to call when done.
        unsafe {
            let notification: id = msg_send![response, notification];
            let request: id = msg_send![notification, request];
            let ident: id = msg_send![request, identifier];
            let utf8 = ident.UTF8String();
            if !utf8.is_null() {
                let ident = std::ffi::CStr::from_ptr(utf8).to_string_lossy();
                if let Some(thread) = ident.strip_prefix("thread-").and_then(|n| n.parse().ok())
                    && let Some(clicks) = CLICKS.get()
                    && let Ok(clicks) = clicks.lock()
                {
                    let _ = clicks.unbounded_send(thread);
                }
            }
            (*(handler as *mut Block<(), ()>)).call(());
        }
    }

    fn string(text: &str) -> id {
        // SAFETY: an autoreleased copy, used within the caller's pool.
        unsafe { NSString::alloc(nil).init_str(text).autorelease() }
    }

    pub fn post(thread: usize, title: &str, subtitle: &str, body: &str) {
        let Some(center) = center() else { return };
        // SAFETY: as for `init`; `content` is released after the request copies it.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let content: id = msg_send![objc::class!(UNMutableNotificationContent), new];
            let _: () = msg_send![content, setTitle: string(title)];
            if !subtitle.is_empty() {
                let _: () = msg_send![content, setSubtitle: string(subtitle)];
            }
            let _: () = msg_send![content, setBody: string(body)];
            let sound: id = msg_send![objc::class!(UNNotificationSound), defaultSound];
            let _: () = msg_send![content, setSound: sound];
            let ident = string(&super::identifier(thread));
            let _: () = msg_send![content, setThreadIdentifier: ident];
            let request: id = msg_send![objc::class!(UNNotificationRequest),
                requestWithIdentifier: ident content: content trigger: nil];
            let _: () = msg_send![center, addNotificationRequest: request withCompletionHandler: nil];
            let _: () = msg_send![content, release];
            pool.drain();
        }
    }

    pub fn withdraw(thread: usize) {
        let Some(center) = center() else { return };
        // SAFETY: as for `post`.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let ids: id = msg_send![objc::class!(NSArray), arrayWithObject: string(&super::identifier(thread))];
            let _: () = msg_send![center, removeDeliveredNotificationsWithIdentifiers: ids];
            pool.drain();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_read_as_plain_text() {
        assert_eq!(excerpt("## Done\n\n- Fixed **the** `bug`\n```rust\nfn x() {}\n```"), "Done Fixed the bug fn x() {}");
        let long = "word ".repeat(100);
        let cut = excerpt(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= 180);
    }
}
