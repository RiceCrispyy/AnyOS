//! Graphics primitives and device context management

/// Color (RGBA)
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub fn with_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Convert from Windows COLORREF (BGR format)
    pub fn from_colorref(colorref: u32) -> Self {
        let b = ((colorref >> 16) & 0xFF) as u8;
        let g = ((colorref >> 8) & 0xFF) as u8;
        let r = (colorref & 0xFF) as u8;
        Self::new(r, g, b)
    }

    /// Convert to Windows COLORREF (BGR format)
    pub fn to_colorref(self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }

    // Common colors
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const RED: Color = Color { r: 255, g: 0, b: 0, a: 255 };
    pub const GREEN: Color = Color { r: 0, g: 255, b: 0, a: 255 };
    pub const BLUE: Color = Color { r: 0, g: 0, b: 255, a: 255 };
    pub const GRAY: Color = Color { r: 128, g: 128, b: 128, a: 255 };
}

/// Rectangle
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

impl Default for Rect {
    fn default() -> Self {
        Self::new(0, 0, 800, 600)
    }
}

/// Point in 2D space
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Drawing context handle (opaque to Win32)
pub type DrawingContext = u64;

/// Pen styles
pub mod pen_styles {
    pub const PS_SOLID: u32 = 0;
    pub const PS_DASH: u32 = 1;
    pub const PS_DOT: u32 = 2;
    pub const PS_DASHDOT: u32 = 3;
    pub const PS_DASHDOTDOT: u32 = 4;
    pub const PS_NULL: u32 = 5;
    pub const PS_INSIDEFRAME: u32 = 6;
}

/// Brush styles
pub mod brush_styles {
    pub const BS_SOLID: u32 = 0;
    pub const BS_NULL: u32 = 1;
    pub const BS_HATCHED: u32 = 2;
    pub const BS_PATTERN: u32 = 3;
    pub const BS_INDEXED: u32 = 4;
    pub const BS_DIBPATTERN: u32 = 5;
    pub const BS_PATTERN8X8: u32 = 7;
}

/// Pen object
#[derive(Debug, Clone)]
pub struct Pen {
    pub style: u32,
    pub width: i32,
    pub color: Color,
}

impl Pen {
    pub fn new(style: u32, width: i32, color: Color) -> Self {
        Self { style, width, color }
    }
}

/// Brush object
#[derive(Debug, Clone)]
pub struct Brush {
    pub style: u32,
    pub color: Color,
}

impl Brush {
    pub fn new(style: u32, color: Color) -> Self {
        Self { style, color }
    }

    pub fn solid(color: Color) -> Self {
        Self::new(brush_styles::BS_SOLID, color)
    }
}

/// Font attributes
#[derive(Debug, Clone)]
pub struct Font {
    pub height: i32,
    pub width: i32,
    pub weight: u32,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub charset: u32,
    pub name: String,
}

impl Font {
    pub fn new(height: i32, name: &str) -> Self {
        Self {
            height,
            width: 0,
            weight: 400, // FW_NORMAL
            italic: false,
            underline: false,
            strikeout: false,
            charset: 0, // ANSI_CHARSET
            name: name.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_conversion() {
        let color = Color::new(255, 128, 64);
        let colorref = color.to_colorref();
        let back = Color::from_colorref(colorref);
        assert_eq!(color, back);
    }

    #[test]
    fn test_rect() {
        let rect = Rect::new(10, 20, 100, 200);
        assert_eq!(rect.width(), 90);
        assert_eq!(rect.height(), 180);
        assert!(rect.contains_point(50, 100));
        assert!(!rect.contains_point(5, 100));
    }
}
