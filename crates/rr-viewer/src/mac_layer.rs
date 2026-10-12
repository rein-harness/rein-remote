//! macOS: the picture scaled by Core Animation on the GPU instead of by the CPU. softbuffer shows its
//! buffer as the contents of a layer it adds to the window's view, at the buffer's own size in the
//! top left; told to fit its contents to its bounds with the aspect kept, that layer takes a buffer
//! the stream's size and scales it as it composites. Handing a Retina window's full 2560×1440 to
//! softbuffer instead cost ~4 ms of scaling and ~12 ms for Core Animation to take the image, a frame.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use std::ffi::c_void;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[link(name = "QuartzCore", kind = "framework")]
extern "C" {
    static kCAGravityResizeAspect: *const AnyObject;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGColorBlack: *const c_void;
    fn CGColorGetConstantColor(name: *const c_void) -> *const c_void;
}

/// Sets softbuffer's layer (the last one on the window's view) to fit its contents with the aspect
/// kept, on black: true when it did (the caller then presents at the picture's size).
pub fn scale_on_gpu(window: &Window) -> bool {
    let Ok(handle) = window.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
    // SAFETY: the handle holds a live NSView for as long as the window lives; these are plain
    // property reads and writes on it and its layers, on the main thread (where winit runs).
    unsafe {
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        let root: *mut AnyObject = msg_send![view, layer];
        if root.is_null() {
            return false;
        }
        let subs: *mut AnyObject = msg_send![root, sublayers];
        if subs.is_null() {
            return false;
        }
        let layer: *mut AnyObject = msg_send![subs, lastObject];
        if layer.is_null() {
            return false;
        }
        let _: () = msg_send![layer, setContentsGravity: kCAGravityResizeAspect];
        let black = CGColorGetConstantColor(kCGColorBlack);
        let _: () = msg_send![layer, setBackgroundColor: black];
        let _: () = msg_send![root, setBackgroundColor: black];
    }
    true
}
