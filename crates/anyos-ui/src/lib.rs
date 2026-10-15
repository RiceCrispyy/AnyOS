//! Universal UI layer for AnyOS
//!
//! Provides platform-agnostic traits for window creation, drawing, and event handling.
//! Backends implement these traits for their respective platforms (macOS/Metal, Linux/Vulkan, Windows/Direct3D).

pub mod platform;
pub mod window;
pub mod graphics;

pub use window::{
    Window, WindowParams, WindowHandle, Message, MessageType,
    DialogTemplate, DialogControl, ControlType,
};
pub use graphics::{DrawingContext, Color, Rect, Point};

use std::time::Duration;
use thiserror::Error;

/// Errors from the UI layer
#[derive(Error, Debug)]
pub enum UIError {
    #[error("Window creation failed: {0}")]
    WindowCreationFailed(String),

    #[error("Drawing context error: {0}")]
    DrawingContextError(String),

    #[error("Platform error: {0}")]
    PlatformError(String),

    #[error("Invalid handle: {0}")]
    InvalidHandle(String),

    #[error("Operation not supported")]
    NotSupported,
}

pub type UIResult<T> = Result<T, UIError>;

/// Platform-agnostic UI backend trait
///
/// Implementations provide:
/// - Window creation and destruction
/// - Drawing contexts and graphics operations
/// - Dialog box creation (Phase 3)
/// - Event loop handling
/// - Platform-specific optimizations
pub trait UIBackend: Send + Sync {
    /// Create a new window
    fn create_window(&mut self, params: WindowParams) -> UIResult<WindowHandle>;

    /// Destroy a window
    fn destroy_window(&mut self, handle: WindowHandle) -> UIResult<()>;

    /// Create a drawing context for a window
    fn create_dc(&mut self, hwnd: WindowHandle) -> UIResult<u64>;

    /// Destroy a drawing context
    fn destroy_dc(&mut self, dc: u64) -> UIResult<()>;

    /// Draw text on a device context
    fn text_out(
        &mut self,
        dc: u64,
        x: i32,
        y: i32,
        text: &str,
        color: Color,
    ) -> UIResult<()>;

    /// Draw a line
    fn line_to(&mut self, dc: u64, x1: i32, y1: i32, x2: i32, y2: i32) -> UIResult<()>;

    /// Draw a rectangle
    fn rectangle(&mut self, dc: u64, rect: Rect, filled: bool) -> UIResult<()>;

    /// Fill a region with a color
    fn fill_rect(&mut self, dc: u64, rect: Rect, color: Color) -> UIResult<()>;

    /// Set a pixel
    fn set_pixel(&mut self, dc: u64, x: i32, y: i32, color: Color) -> UIResult<()>;

    /// Process one message from the queue
    /// Returns None if no messages available and timeout expires
    fn pump_message(&mut self, timeout: Duration) -> UIResult<Option<Message>>;

    /// Get the window associated with a handle
    fn get_window(&self, handle: WindowHandle) -> UIResult<Window>;

    /// Update window title
    fn set_window_title(&mut self, handle: WindowHandle, title: &str) -> UIResult<()>;

    /// Show or hide a window
    fn show_window(&mut self, handle: WindowHandle, visible: bool) -> UIResult<()>;

    /// Get window rectangle
    fn get_window_rect(&self, handle: WindowHandle) -> UIResult<Rect>;

    /// Move/resize window
    fn set_window_rect(&mut self, handle: WindowHandle, rect: Rect) -> UIResult<()>;

    /// Create a modal dialog box (Phase 3)
    /// Returns a dialog handle and sets the result code when closed
    fn create_dialog_modal(
        &mut self,
        parent: WindowHandle,
        template: &DialogTemplate,
    ) -> UIResult<i32> {
        let _ = (parent, template);
        Err(UIError::NotSupported)
    }

    /// Create a modeless dialog box (Phase 3)
    /// Returns a handle to the dialog
    fn create_dialog_modeless(
        &mut self,
        parent: WindowHandle,
        template: &DialogTemplate,
    ) -> UIResult<WindowHandle> {
        let _ = (parent, template);
        Err(UIError::NotSupported)
    }

    /// Destroy a dialog box (Phase 3)
    fn destroy_dialog(&mut self, handle: WindowHandle) -> UIResult<()> {
        let _ = handle;
        Err(UIError::NotSupported)
    }

    /// End a modal dialog with a result code (Phase 3)
    fn end_dialog(&mut self, handle: WindowHandle, result: i32) -> UIResult<()> {
        let _ = (handle, result);
        Err(UIError::NotSupported)
    }
}

/// Global UI state manager
/// This will be stored in the kernel and accessed through call gate
pub struct UIManager {
    backend: Option<Box<dyn UIBackend>>,
    window_count: u32,
}

impl UIManager {
    pub fn new() -> Self {
        Self {
            backend: None,
            window_count: 0,
        }
    }

    /// Initialize the backend for this platform
    pub fn init_platform_backend(&mut self) -> UIResult<()> {
        // Platform-specific initialization happens here
        #[cfg(target_os = "macos")]
        {
            self.backend = Some(Box::new(platform::macos::MacOSBackend::new()?));
        }

        #[cfg(target_os = "linux")]
        {
            self.backend = Some(Box::new(platform::linux::LinuxBackend::new()?));
        }

        #[cfg(target_os = "windows")]
        {
            self.backend = Some(Box::new(platform::windows::WindowsBackend::new()?));
        }

        Ok(())
    }

    pub fn backend(&self) -> Option<&dyn UIBackend> {
        self.backend.as_ref().map(|b| b.as_ref())
    }

    pub fn backend_mut(&mut self) -> Option<&mut dyn UIBackend> {
        self.backend.as_mut().map(|b| b.as_mut())
    }
}

impl Default for UIManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ui_manager_creation() {
        let manager = UIManager::new();
        assert!(manager.backend.is_none());
    }
}
