//! Windows implementation using Direct3D/Win32

use crate::{graphics::Color, window::WindowHandle, UIBackend, UIError, UIResult};
use std::time::Duration;

/// Placeholder for Windows backend
pub struct WindowsBackend;

impl WindowsBackend {
    pub fn new() -> UIResult<Self> {
        Err(UIError::NotSupported)
    }
}

impl UIBackend for WindowsBackend {
    // TODO: Implement Windows backend with Direct3D
    fn create_window(&mut self, _: crate::window::WindowParams) -> UIResult<WindowHandle> {
        Err(UIError::NotSupported)
    }
    fn destroy_window(&mut self, _: WindowHandle) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn create_dc(&mut self, _: WindowHandle) -> UIResult<u64> {
        Err(UIError::NotSupported)
    }
    fn destroy_dc(&mut self, _: u64) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn text_out(&mut self, _: u64, _: i32, _: i32, _: &str, _: Color) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn line_to(&mut self, _: u64, _: i32, _: i32, _: i32, _: i32) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn rectangle(&mut self, _: u64, _: crate::graphics::Rect, _: bool) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn fill_rect(&mut self, _: u64, _: crate::graphics::Rect, _: Color) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn set_pixel(&mut self, _: u64, _: i32, _: i32, _: Color) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn pump_message(&mut self, _: Duration) -> UIResult<Option<crate::window::Message>> {
        Err(UIError::NotSupported)
    }
    fn get_window(&self, _: WindowHandle) -> UIResult<crate::window::Window> {
        Err(UIError::NotSupported)
    }
    fn set_window_title(&mut self, _: WindowHandle, _: &str) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn show_window(&mut self, _: WindowHandle, _: bool) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
    fn get_window_rect(&self, _: WindowHandle) -> UIResult<crate::graphics::Rect> {
        Err(UIError::NotSupported)
    }
    fn set_window_rect(&mut self, _: WindowHandle, _: crate::graphics::Rect) -> UIResult<()> {
        Err(UIError::NotSupported)
    }
}
