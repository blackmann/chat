//! Moving files to the Trash, so a discard that drops a file Git never kept
//! can still be taken back from Finder.
use std::path::Path;

#[cfg(all(target_os = "macos", not(test)))]
#[allow(unexpected_cfgs)] // objc's macros test cfgs of their own.
pub fn trash(file: &Path) -> Result<(), String> {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::runtime::{BOOL, NO};
    use objc::{class, msg_send, sel, sel_impl};

    let text = file.to_str().ok_or("The path isn't valid UTF-8.")?;
    // SAFETY: autoreleased objects, all used inside this pool; NSFileManager's
    // shared instance is safe to use off the main thread.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let path = NSString::alloc(nil).init_str(text).autorelease();
        let url: id = msg_send![class!(NSURL), fileURLWithPath: path];
        let manager: id = msg_send![class!(NSFileManager), defaultManager];
        let mut error: id = nil;
        let ok: BOOL = msg_send![manager, trashItemAtURL: url resultingItemURL: nil error: &mut error];
        let result = if ok == NO {
            let reason = if error == nil {
                "unknown error".to_owned()
            } else {
                let description: id = msg_send![error, localizedDescription];
                std::ffi::CStr::from_ptr(description.UTF8String()).to_string_lossy().into_owned()
            };
            Err(reason)
        } else {
            Ok(())
        };
        pool.drain();
        result
    }
}

/// Tests, and other platforms, delete outright.
#[cfg(any(not(target_os = "macos"), test))]
pub fn trash(file: &Path) -> Result<(), String> {
    let result = if file.is_dir() && !file.is_symlink() { std::fs::remove_dir_all(file) } else { std::fs::remove_file(file) };
    result.map_err(|e| e.to_string())
}
