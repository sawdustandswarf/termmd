//! `dlopen`, for the Vulkan loader.

use std::ffi::{CStr, c_void};
use std::ptr::NonNull;

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub const RTLD_NOW: c_int = 2;
    pub const RTLD_LOCAL: c_int = 0;

    unsafe extern "C" {
        pub fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub fn dlclose(handle: *mut c_void) -> c_int;
        pub fn dlerror() -> *mut c_char;
    }
}

/// A shared library opened with `dlopen`, closed on drop.
#[derive(Debug)]
pub struct Library {
    handle: NonNull<c_void>,
}

fn dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated string valid until the
    // next dl* call on this thread; we copy it out immediately.
    let msg = unsafe { ffi::dlerror() };
    if msg.is_null() {
        "unknown error".to_owned()
    } else {
        // SAFETY: non-null, NUL-terminated (above).
        unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
    }
}

impl Library {
    pub fn open(name: &CStr) -> Result<Library, String> {
        // SAFETY: `name` is NUL-terminated. Loading runs the library's
        // initialisers, which is the point.
        let handle = unsafe { ffi::dlopen(name.as_ptr(), ffi::RTLD_NOW | ffi::RTLD_LOCAL) };
        NonNull::new(handle).map(|handle| Library { handle }).ok_or_else(dl_error)
    }

    /// The address of `symbol`, if the library exports it.
    pub fn symbol(&self, symbol: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: valid handle (we own it) and NUL-terminated name.
        NonNull::new(unsafe { ffi::dlsym(self.handle.as_ptr(), symbol.as_ptr()) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle came from dlopen and is closed exactly once.
        unsafe { ffi::dlclose(self.handle.as_ptr()) };
    }
}
