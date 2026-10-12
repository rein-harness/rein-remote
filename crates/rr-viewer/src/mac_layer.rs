//! macOS: the picture scaled by Core Animation on the GPU instead of by the CPU. softbuffer shows its
//! buffer as the contents of a layer it adds to the window's view, at the buffer's own size in the
//! top left; told to fit its contents to its bounds with the aspect kept, that layer takes a buffer
//! the stream's size and scales it as it composites. Handing a Retina window's full 2560×1440 to
//! softbuffer instead cost ~4 ms of scaling and ~12 ms for Core Animation to take the image, a frame.

use crate::decode::vt::Surface;
use crate::present::CursorImage;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo};
use objc2_quartz_core::{kCAGravityResize, kCAGravityResizeAspect, CALayer, CATransaction};
use std::ffi::c_void;
use std::ptr::NonNull;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

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
        let gravity: &AnyObject = kCAGravityResizeAspect;
        let _: () = msg_send![layer, setContentsGravity: gravity];
        let black = CGColorGetConstantColor(kCGColorBlack);
        let _: () = msg_send![layer, setBackgroundColor: black];
        let _: () = msg_send![root, setBackgroundColor: black];
    }
    true
}

/// The picture straight from the decoder: a layer whose contents are the decoder's IOSurface (no
/// copy through the CPU: Core Animation scales and composites it on the GPU), and the remote
/// pointer as a small layer of its own above it. Both sit above softbuffer's layer, which shows
/// what isn't the picture (the list of machines, the connection's state) when this is hidden.
pub struct VideoLayer {
    root: Retained<CALayer>,
    video: Retained<CALayer>,
    pointer: Retained<CALayer>,
    /// The picture on screen, held while it's shown (the decoder reuses it once let go).
    shown: Option<Surface>,
    pointer_size: (f64, f64),
}

impl VideoLayer {
    pub fn new(window: &Window) -> Option<Self> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
        // SAFETY: the handle holds a live NSView; its layer (softbuffer made it layer-backed).
        let root: Option<Retained<CALayer>> = unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            msg_send![view, layer]
        };
        let root = root?;
        let (video, pointer) = (CALayer::new(), CALayer::new());
        unsafe {
            video.setContentsGravity(kCAGravityResizeAspect);
            pointer.setContentsGravity(kCAGravityResize);
        }
        video.setHidden(true);
        pointer.setHidden(true);
        root.addSublayer(&video);
        root.addSublayer(&pointer);
        Some(Self {
            root,
            video,
            pointer,
            shown: None,
            pointer_size: (0.0, 0.0),
        })
    }

    /// Changes go in at once, not animated (a layer's default is a quarter-second fade).
    fn change(f: impl FnOnce()) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        f();
        CATransaction::commit();
    }

    /// Shows `s` (when it's a new picture), over the whole view.
    pub fn show(&mut self, s: Option<&Surface>) {
        let bounds = self.root.bounds();
        Self::change(|| {
            self.video.setFrame(bounds);
            if let Some(s) = s {
                // SAFETY: an IOSurfaceRef is an IOSurface object, which a layer takes as contents.
                unsafe { self.video.setContents(Some(&*(s.io_surface() as *const AnyObject))) };
            }
            self.video.setHidden(false);
        });
        if let Some(s) = s {
            self.shown = Some(s.clone());
        }
    }

    pub fn hide(&mut self) {
        Self::change(|| {
            self.video.setHidden(true);
            self.pointer.setHidden(true);
        });
        self.shown = None;
    }

    /// The pointer's image (RGBA, straight alpha), or none.
    pub fn pointer_image(&mut self, c: Option<&CursorImage>) {
        let image = c.and_then(|c| {
            let rgba = c.rgba.clone().into_boxed_slice();
            let len = rgba.len();
            let data = Box::into_raw(rgba) as *mut c_void;
            unsafe extern "C-unwind" fn release(_info: *mut c_void, data: NonNull<c_void>, size: usize) {
                // SAFETY: the box made just above, of this size.
                drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(data.as_ptr() as *mut u8, size)) })
            }
            // SAFETY: `data` is `len` bytes, freed by `release` when the provider is done with it.
            let provider = unsafe { CGDataProvider::with_data(std::ptr::null_mut(), data, len, Some(release)) }?;
            let space = CGColorSpace::new_device_rgb()?;
            unsafe {
                CGImage::new(
                    c.w,
                    c.h,
                    8,
                    32,
                    c.w * 4,
                    Some(&space),
                    CGBitmapInfo(CGImageAlphaInfo::Last.0),
                    Some(&provider),
                    std::ptr::null(),
                    false,
                    CGColorRenderingIntent::RenderingIntentDefault,
                )
            }
        });
        Self::change(|| unsafe {
            self.pointer.setContents(
                image
                    .as_ref()
                    .map(|i| &*(objc2_core_foundation::CFRetained::as_ptr(i).as_ptr() as *const AnyObject)),
            );
            if image.is_none() {
                self.pointer.setHidden(true);
            }
        });
        self.pointer_size = c.map_or((0.0, 0.0), |c| (c.w as f64, c.h as f64));
    }

    /// The pointer with its top left at `x`,`y` (points from the view's top left), `k` points to its
    /// pixels; hidden when `visible` is false.
    pub fn place_pointer(&self, x: f64, y: f64, k: f64, visible: bool) {
        let bounds = self.root.bounds();
        let (w, h) = (self.pointer_size.0 * k, self.pointer_size.1 * k);
        // The view's layer counts up from its bottom.
        let frame = CGRect::new(CGPoint::new(x, bounds.size.height - y - h), CGSize::new(w, h));
        Self::change(|| {
            self.pointer.setFrame(frame);
            self.pointer.setHidden(!visible || w == 0.0);
        });
    }
}
