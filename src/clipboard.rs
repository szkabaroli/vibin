//! System clipboard access for the editor's copy/cut/paste. Uses `arboard`
//! (NSPasteboard on macOS, X11/Wayland on Linux, the Win32 clipboard on
//! Windows). Under `cfg(test)` it swaps in an in-memory buffer so tests are
//! deterministic and never disturb the user's real clipboard.

#[cfg(not(test))]
pub fn set(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_owned());
    }
}

#[cfg(not(test))]
pub fn get() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

#[cfg(test)]
mod mock {
    use std::sync::Mutex;
    static BUF: Mutex<String> = Mutex::new(String::new());
    pub fn set(text: &str) {
        *BUF.lock().unwrap() = text.to_string();
    }
    pub fn get() -> Option<String> {
        let b = BUF.lock().unwrap();
        (!b.is_empty()).then(|| b.clone())
    }
}

#[cfg(test)]
pub use mock::{get, set};
