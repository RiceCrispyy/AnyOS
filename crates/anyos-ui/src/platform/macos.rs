//! macOS implementation using Cocoa and Metal
//!
//! This module provides native macOS window creation and rendering using NSWindow
//! and Cocoa APIs.

use crate::{
    graphics::{Color, Rect},
    window::{WindowHandle, WindowParams, Window, Message, MessageType},
    UIBackend, UIError, UIResult,
};
use std::collections::HashMap;
use std::time::Duration;
use objc::{class, msg_send};
use objc::runtime::Object;
use objc_foundation::{NSString, INSString};
use cocoa::foundation::{NSRect, NSPoint, NSSize};
use cocoa::appkit::{NSWindow, NSWindowStyleMask, NSBackingStoreType};

/// macOS backend using Cocoa + Metal
pub struct MacOSBackend {
    windows: HashMap<WindowHandle, MacOSWindow>,
    next_handle: u64,
    message_queue: std::collections::VecDeque<Message>,
}

struct MacOSWindow {
    handle: WindowHandle,
    title: String,
    class: String,
    visible: bool,
    rect: Rect,
    ns_window: Option<*mut NSWindow>,
}

// Raw pointers to Cocoa objects are not Send/Sync by default,
// but we're managing them safely in this context
unsafe impl Send for MacOSWindow {}
unsafe impl Sync for MacOSWindow {}

impl MacOSWindow {
    fn new(handle: WindowHandle, title: String, class: String, rect: Rect) -> Self {
        Self {
            handle,
            title,
            class,
            visible: false,
            rect,
            ns_window: None,
        }
    }
}

impl Drop for MacOSWindow {
    fn drop(&mut self) {
        // Release the NSWindow if it exists
        if let Some(window_ptr) = self.ns_window {
            unsafe {
                let _: () = msg_send![window_ptr, release];
            }
        }
    }
}

impl MacOSBackend {
    pub fn new() -> UIResult<Self> {
        // Initialize Cocoa autorelease pool
        unsafe {
            let _pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
            let _ = _pool;
        }

        Ok(Self {
            windows: HashMap::new(),
            next_handle: 0x1000,
            message_queue: std::collections::VecDeque::new(),
        })
    }

    fn next_handle(&mut self) -> WindowHandle {
        let h = WindowHandle(self.next_handle);
        self.next_handle += 4;
        h
    }

    fn create_ns_window(
        &self,
        title: &str,
        rect: Rect,
        style_mask: NSWindowStyleMask,
    ) -> UIResult<*mut NSWindow> {
        unsafe {
            // Create NSRect from our Rect
            let ns_rect = NSRect::new(
                NSPoint::new(rect.left as f64, rect.top as f64),
                NSSize::new(
                    rect.width() as f64,
                    rect.height() as f64,
                ),
            );

            // Allocate NSWindow
            let window: *mut NSWindow = msg_send![class!(NSWindow), alloc];
            if window.is_null() {
                return Err(UIError::WindowCreationFailed(
                    "Failed to allocate NSWindow".to_string(),
                ));
            }

            // Initialize with content rect, style mask, backing, and defer
            let window: *mut NSWindow = msg_send![
                window,
                initWithContentRect: ns_rect
                styleMask: style_mask
                backing: 1u64  // NSBackingStoreBuffered
                defer: false
            ];

            if window.is_null() {
                return Err(UIError::WindowCreationFailed(
                    "Failed to initialize NSWindow".to_string(),
                ));
            }

            // Set window title
            let ns_title = NSString::from_str(title);
            let _: () = msg_send![window, setTitle: ns_title];

            // Make window key and order to front
            let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<Object>()];

            Ok(window)
        }
    }
}

impl UIBackend for MacOSBackend {
    fn create_window(&mut self, params: WindowParams) -> UIResult<WindowHandle> {
        let handle = self.next_handle();
        let visible = (params.style & 0x10000000) != 0; // WS_VISIBLE

        // Build NSWindowStyleMask from Windows style flags
        let style_mask = NSWindowStyleMask::NSWindowStyleMaskTitled
            | NSWindowStyleMask::NSWindowStyleMaskClosable
            | NSWindowStyleMask::NSWindowStyleMaskMiniaturizable
            | NSWindowStyleMask::NSWindowStyleMaskResizable;

        // Create native NSWindow
        let ns_window = self.create_ns_window(&params.title, params.rect, style_mask)?;

        // Create our wrapper
        let mut window = MacOSWindow::new(
            handle,
            params.title.clone(),
            params.class.clone(),
            params.rect,
        );
        window.ns_window = Some(ns_window);
        window.visible = visible;

        // Show window if WS_VISIBLE is set
        if visible {
            unsafe {
                let _: () = msg_send![ns_window, orderFront: std::ptr::null_mut::<Object>()];
            }
        }

        self.windows.insert(handle, window);

        // Post a paint message so the window can be rendered
        self.message_queue.push_back(Message::new(handle, MessageType::Paint));

        Ok(handle)
    }

    fn destroy_window(&mut self, handle: WindowHandle) -> UIResult<()> {
        if let Some(window) = self.windows.remove(&handle) {
            if let Some(ns_window) = window.ns_window {
                unsafe {
                    // Close the window
                    let _: () = msg_send![ns_window, close];
                }
            }
            Ok(())
        } else {
            Err(UIError::InvalidHandle(format!("0x{:x}", handle.0)))
        }
    }

    fn create_dc(&mut self, hwnd: WindowHandle) -> UIResult<u64> {
        if !self.windows.contains_key(&hwnd) {
            return Err(UIError::InvalidHandle(format!("0x{:x}", hwnd.0)));
        }
        // Return a fake DC handle (rendering will use Metal/CoreGraphics)
        Ok(0x2000)
    }

    fn destroy_dc(&mut self, _dc: u64) -> UIResult<()> {
        // DC cleanup happens automatically
        Ok(())
    }

    fn text_out(
        &mut self,
        _dc: u64,
        _x: i32,
        _y: i32,
        _text: &str,
        _color: Color,
    ) -> UIResult<()> {
        // Phase 2: Text rendering with CoreText
        Ok(())
    }

    fn line_to(&mut self, _dc: u64, _x1: i32, _y1: i32, _x2: i32, _y2: i32) -> UIResult<()> {
        // Phase 2: Line rendering with CoreGraphics
        Ok(())
    }

    fn rectangle(&mut self, _dc: u64, _rect: Rect, _filled: bool) -> UIResult<()> {
        // Phase 2: Rectangle rendering with CoreGraphics
        Ok(())
    }

    fn fill_rect(&mut self, _dc: u64, _rect: Rect, _color: Color) -> UIResult<()> {
        // Phase 2: Fill rendering with CoreGraphics
        Ok(())
    }

    fn set_pixel(&mut self, _dc: u64, _x: i32, _y: i32, _color: Color) -> UIResult<()> {
        // Phase 2: Pixel-level drawing
        Ok(())
    }

    fn pump_message(&mut self, _timeout: Duration) -> UIResult<Option<Message>> {
        // Basic event pump - return queued messages
        // Phase 2: Integrate with Cocoa event loop
        Ok(self.message_queue.pop_front())
    }

    fn get_window(&self, handle: WindowHandle) -> UIResult<Window> {
        let window = self
            .windows
            .get(&handle)
            .ok_or_else(|| UIError::InvalidHandle(format!("0x{:x}", handle.0)))?;

        Ok(Window {
            handle,
            class: window.class.clone(),
            title: window.title.clone(),
            visible: window.visible,
            enabled: true,
            parent: WindowHandle(0),
        })
    }

    fn set_window_title(&mut self, handle: WindowHandle, title: &str) -> UIResult<()> {
        let window = self
            .windows
            .get_mut(&handle)
            .ok_or_else(|| UIError::InvalidHandle(format!("0x{:x}", handle.0)))?;

        window.title = title.to_string();

        // Update NSWindow title if window exists
        if let Some(ns_window) = window.ns_window {
            unsafe {
                let ns_title = NSString::from_str(title);
                let _: () = msg_send![ns_window, setTitle: ns_title];
            }
        }

        Ok(())
    }

    fn show_window(&mut self, handle: WindowHandle, visible: bool) -> UIResult<()> {
        let window = self
            .windows
            .get_mut(&handle)
            .ok_or_else(|| UIError::InvalidHandle(format!("0x{:x}", handle.0)))?;

        window.visible = visible;

        // Show or hide the NSWindow
        if let Some(ns_window) = window.ns_window {
            unsafe {
                if visible {
                    let _: () = msg_send![ns_window, orderFront: std::ptr::null_mut::<Object>()];
                } else {
                    let _: () = msg_send![ns_window, orderOut: std::ptr::null_mut::<Object>()];
                }
            }
        }

        Ok(())
    }

    fn get_window_rect(&self, handle: WindowHandle) -> UIResult<Rect> {
        let window = self
            .windows
            .get(&handle)
            .ok_or_else(|| UIError::InvalidHandle(format!("0x{:x}", handle.0)))?;

        Ok(window.rect)
    }

    fn set_window_rect(&mut self, handle: WindowHandle, rect: Rect) -> UIResult<()> {
        let window = self
            .windows
            .get_mut(&handle)
            .ok_or_else(|| UIError::InvalidHandle(format!("0x{:x}", handle.0)))?;

        window.rect = rect;

        // Move and resize the NSWindow
        if let Some(ns_window) = window.ns_window {
            unsafe {
                let ns_rect = NSRect::new(
                    NSPoint::new(rect.left as f64, rect.top as f64),
                    NSSize::new(rect.width() as f64, rect.height() as f64),
                );
                let _: () = msg_send![ns_window, setFrame: ns_rect display: true];
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_creation() {
        let backend = MacOSBackend::new();
        assert!(backend.is_ok());
    }

    #[test]
    fn test_window_creation() {
        let mut backend = MacOSBackend::new().unwrap();
        let params = WindowParams::default();
        let result = backend.create_window(params);
        assert!(result.is_ok());
    }
}
