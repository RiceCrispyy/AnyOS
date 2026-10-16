//! Stub implementation for unsupported platforms

use crate::{GraphicsContext, UIBackend, Window, WindowConfig};
use std::sync::Arc;

/// Stub UI backend for platforms without real implementation
pub struct StubBackend;

impl UIBackend for StubBackend {
    type Window = StubWindow;
    type GraphicsContext = StubGraphicsContext;

    fn create_window(
        &self,
        config: WindowConfig,
    ) -> Result<Arc<Self::Window>, Box<dyn std::error::Error>> {
        Ok(Arc::new(StubWindow { config }))
    }

    fn create_graphics_context(&self) -> Result<Self::GraphicsContext, Box<dyn std::error::Error>> {
        Ok(StubGraphicsContext)
    }

    fn process_events(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

/// Stub window implementation
pub struct StubWindow {
    config: WindowConfig,
}

impl Window for StubWindow {
    fn show(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn hide(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn set_title(&self, _title: &str) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn get_title(&self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(String::new())
    }

    fn set_position(&self, _x: i32, _y: i32) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn get_position(&self) -> Result<(i32, i32), Box<dyn std::error::Error>> {
        Ok((0, 0))
    }

    fn set_size(&self, _width: u32, _height: u32) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn get_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        Ok((self.config.width, self.config.height))
    }

    fn close(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

/// Stub graphics context implementation
pub struct StubGraphicsContext;

impl GraphicsContext for StubGraphicsContext {
    fn clear(&mut self, _r: f32, _g: f32, _b: f32) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn present(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
