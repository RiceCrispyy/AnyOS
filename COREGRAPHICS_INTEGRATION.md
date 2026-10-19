# CoreGraphics and CoreText Integration Guide

## Overview

This document provides detailed technical guidance for integrating CoreGraphics and CoreText into the Phase 2 graphics rendering system.

## Drawing Context Setup

### NSView Subclass Pattern

The recommended approach uses an NSView subclass to handle drawing:

```rust
use objc::class;
use objc::runtime::Class;
use objc::{msg_send, sel, sel_impl};

pub struct DrawingView {
    ns_view: *mut Object,
    backend_ptr: *mut MacOSBackend,
    dc: u64,
}

impl DrawingView {
    /// Create a new drawing view
    pub fn new(backend: &mut MacOSBackend, dc: u64) -> UIResult<Self> {
        unsafe {
            // Create NSView subclass dynamically
            let superclass = class!(NSView);
            let mut class_decl = ClassDecl::new("DrawingView", superclass)
                .ok_or_else(|| UIError::WindowCreationFailed(
                    "Failed to declare DrawingView class".to_string()
                ))?;

            // Add drawRect: method
            class_decl.add_method(
                sel!(drawRect:),
                Self::draw_rect as extern "C" fn(&mut Object, Sel, NSRect) -> ()
            );

            let class = class_decl.register();
            let view: *mut Object = msg_send![class, alloc];
            let _: () = msg_send![view, init];

            Ok(DrawingView {
                ns_view: view,
                backend_ptr: backend as *mut MacOSBackend,
                dc,
            })
        }
    }

    extern "C" fn draw_rect(this: &mut Object, _cmd: Sel, rect: NSRect) {
        unsafe {
            // Implementation would go here
        }
    }
}
```

### CoreGraphics Context Access

Get the current graphics context during drawing:

```rust
unsafe {
    // Method 1: From NSGraphicsContext
    let ns_context: *mut Object = msg_send![class!(NSGraphicsContext), currentContext];
    let cg_context: *mut CGContext = msg_send![ns_context, CGContext];
    
    // Method 2: Direct in drawRect:
    let cg_context: *mut CGContext = msg_send![
        class!(NSGraphicsContext),
        graphicsContextWithWindow:window
    ];
}
```

## Text Rendering with CoreText

### Complete Text Rendering Implementation

```rust
use core_foundation::string::CFString;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_graphics::context::CGContext;

fn render_text(
    context: &mut CGContext,
    x: i32,
    y: i32,
    text: &str,
    color: Color,
    font_name: &str,
    font_size: f32,
) -> UIResult<()> {
    unsafe {
        // 1. Create font descriptor
        let font_name_cf = CFString::new(font_name);
        let font_size_cf = CFNumber::from_f64(font_size as f64);
        
        // 2. Create attributes dictionary
        let attrs = [
            (CFString::new("NSFontNameAttribute"), font_name_cf.as_CFTypeRef()),
            (CFString::new("NSFontSizeAttribute"), font_size_cf.as_CFTypeRef()),
        ];
        let attr_dict = CFDictionary::from_CFArray(&attrs);

        // 3. Create attributed string
        let text_cf = CFString::new(text);
        let attr_string = CFAttributedStringRef::from_dictionary(&text_cf, &attr_dict)?;

        // 4. Create CTLine
        let ct_line = CTLineRef::from_attributed_string(&attr_string)?;

        // 5. Set color
        let color_cg = color_to_cgcolor(color);
        context.set_stroke_color_with_color(&color_cg);
        context.set_fill_color_with_color(&color_cg);

        // 6. Set text position and draw
        context.text_position(x as f64, y as f64);
        ct_line.draw(context);

        Ok(())
    }
}
```

### Font Caching

For performance, cache font objects:

```rust
use std::collections::HashMap;

pub struct FontCache {
    fonts: HashMap<String, CGFont>,
}

impl FontCache {
    pub fn new() -> Self {
        FontCache {
            fonts: HashMap::new(),
        }
    }

    pub fn get_font(&mut self, name: &str) -> UIResult<&CGFont> {
        if !self.fonts.contains_key(name) {
            let font_name = CFString::new(name);
            let font = CGFont::from_name(&font_name)
                .ok_or_else(|| UIError::FontNotFound(name.to_string()))?;
            self.fonts.insert(name.to_string(), font);
        }
        Ok(&self.fonts[name])
    }

    pub fn get_ct_font(&self, name: &str, size: f64) -> UIResult<CTFont> {
        if let Some(cg_font) = self.fonts.get(name) {
            Ok(CTFont::new(cg_font, size))
        } else {
            Err(UIError::FontNotFound(name.to_string()))
        }
    }
}
```

## Line Drawing

### Direct CoreGraphics Implementation

```rust
fn render_line(
    context: &mut CGContext,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: Color,
    width: i32,
) {
    unsafe {
        // Set line width
        context.set_line_width(width as f64);

        // Set stroke color
        let color_cg = color_to_cgcolor(color);
        context.set_stroke_color_with_color(&color_cg);

        // Draw line
        context.move_to_point(x1 as f64, y1 as f64);
        context.add_line_to_point(x2 as f64, y2 as f64);
        context.stroke_path();
    }
}
```

### Advanced Line Styles

```rust
fn render_line_with_style(
    context: &mut CGContext,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: Color,
    width: i32,
    style: LineStyle,
) {
    unsafe {
        context.set_line_width(width as f64);
        let color_cg = color_to_cgcolor(color);
        context.set_stroke_color_with_color(&color_cg);

        // Set line dash pattern
        match style {
            LineStyle::Solid => {
                // No dashing
            }
            LineStyle::Dashed => {
                let lengths: [f64; 2] = [4.0, 2.0];
                context.set_line_dash(0.0, &lengths);
            }
            LineStyle::Dotted => {
                let lengths: [f64; 2] = [1.0, 2.0];
                context.set_line_dash(0.0, &lengths);
            }
        }

        context.move_to_point(x1 as f64, y1 as f64);
        context.add_line_to_point(x2 as f64, y2 as f64);
        context.stroke_path();
    }
}
```

## Rectangle Drawing

### Filled and Stroked Rectangles

```rust
fn render_rectangle(
    context: &mut CGContext,
    rect: Rect,
    filled: bool,
    color: Color,
    width: i32,
) {
    unsafe {
        let cgrect = CGRect::new(
            &CGPoint::new(rect.left as f64, rect.top as f64),
            &CGSize::new(rect.width() as f64, rect.height() as f64),
        );

        let color_cg = color_to_cgcolor(color);

        if filled {
            context.set_fill_color_with_color(&color_cg);
            context.fill_rect(cgrect);
        } else {
            context.set_stroke_color_with_color(&color_cg);
            context.set_line_width(width as f64);
            context.stroke_rect(cgrect);
        }
    }
}
```

### Rounded Rectangles

```rust
fn render_rounded_rect(
    context: &mut CGContext,
    rect: Rect,
    radius: f64,
    filled: bool,
    color: Color,
) {
    unsafe {
        let cgrect = CGRect::new(
            &CGPoint::new(rect.left as f64, rect.top as f64),
            &CGSize::new(rect.width() as f64, rect.height() as f64),
        );

        // Create path with rounded corners
        context.add_path(&CGPath::with_rounded_rect(cgrect, radius, radius, None));

        let color_cg = color_to_cgcolor(color);
        if filled {
            context.set_fill_color_with_color(&color_cg);
            context.fill_path();
        } else {
            context.set_stroke_color_with_color(&color_cg);
            context.stroke_path();
        }
    }
}
```

## Color Management

### NSColor Integration

```rust
use cocoa::appkit::NSColor;

fn create_nscolor(color: Color) -> *mut NSColor {
    unsafe {
        let r = (color.r as f64) / 255.0;
        let g = (color.g as f64) / 255.0;
        let b = (color.b as f64) / 255.0;
        let a = (color.a as f64) / 255.0;

        let color: *mut NSColor = msg_send![
            class!(NSColor),
            colorWithRed:r green:g blue:b alpha:a
        ];
        color
    }
}

fn create_cg_color(color: Color) -> core_graphics::color::CGColor {
    let r = (color.r as f64) / 255.0;
    let g = (color.g as f64) / 255.0;
    let b = (color.b as f64) / 255.0;
    let a = (color.a as f64) / 255.0;
    core_graphics::color::CGColor::rgba(r, g, b, a)
}
```

### Color Space Management

```rust
use core_graphics::color_space::CGColorSpace;

fn create_rgb_color_space() -> CGColorSpace {
    CGColorSpace::create_device_rgb()
}

fn create_srgb_color_space() -> CGColorSpace {
    CGColorSpace::create_with_name(kCGColorSpaceSRGB)
}
```

## Path Drawing

### Complex Shapes

```rust
fn render_polygon(
    context: &mut CGContext,
    points: &[(i32, i32)],
    filled: bool,
    color: Color,
) {
    unsafe {
        if points.is_empty() {
            return;
        }

        // Create path
        let path = CGMutablePath::new();
        
        // Move to first point
        let (x, y) = points[0];
        path.move_to_point(None, x as f64, y as f64);

        // Add lines to remaining points
        for (x, y) in &points[1..] {
            path.add_line_to_point(None, *x as f64, *y as f64);
        }
        
        // Close path
        path.close_subpath();

        context.add_path(&path);

        let color_cg = color_to_cgcolor(color);
        if filled {
            context.set_fill_color_with_color(&color_cg);
            context.fill_path();
        } else {
            context.set_stroke_color_with_color(&color_cg);
            context.stroke_path();
        }
    }
}
```

## Clipping Regions

### Rectangular Clipping

```rust
fn set_clip_rect(context: &mut CGContext, rect: Rect) {
    unsafe {
        let cgrect = CGRect::new(
            &CGPoint::new(rect.left as f64, rect.top as f64),
            &CGSize::new(rect.width() as f64, rect.height() as f64),
        );
        context.clip_to_rect(cgrect);
    }
}
```

### Path-Based Clipping

```rust
fn set_clip_path(context: &mut CGContext, path: &CGPath) {
    unsafe {
        context.add_path(path);
        context.clip();
    }
}
```

## Transforms

### Basic Transformations

```rust
use core_graphics::geometry::CGAffineTransform;

fn apply_transform(context: &mut CGContext, transform: CGAffineTransform) {
    unsafe {
        context.concat_ctm(transform);
    }
}

// Rotation
fn rotate(angle_degrees: f64) -> CGAffineTransform {
    let radians = angle_degrees * std::f64::consts::PI / 180.0;
    CGAffineTransform::make_rotation(radians)
}

// Translation
fn translate(x: f64, y: f64) -> CGAffineTransform {
    CGAffineTransform::make_translation(x, y)
}

// Scale
fn scale(sx: f64, sy: f64) -> CGAffineTransform {
    CGAffineTransform::make_scale(sx, sy)
}
```

## Performance Optimization

### Batching Operations

```rust
fn render_batch(
    context: &mut CGContext,
    operations: &[DrawingOp],
) {
    unsafe {
        // Save graphics state
        context.save_gstate();

        // Render all operations
        for op in operations {
            match op {
                DrawingOp::Line { x1, y1, x2, y2, color, width } => {
                    render_line(context, *x1, *y1, *x2, *y2, *color, *width);
                }
                DrawingOp::Rectangle { rect, filled, color, width } => {
                    render_rectangle(context, *rect, *filled, *color, *width);
                }
                // ... other operations
                _ => {}
            }
        }

        // Restore graphics state
        context.restore_gstate();
    }
}
```

### Double Buffering

```rust
pub struct DoubleBuffer {
    front: CGImage,
    back: CGImage,
}

impl DoubleBuffer {
    pub fn new(width: i32, height: i32) -> UIResult<Self> {
        unsafe {
            // Create two offscreen contexts
            let front = create_offscreen_image(width, height)?;
            let back = create_offscreen_image(width, height)?;

            Ok(DoubleBuffer { front, back })
        }
    }

    pub fn swap(&mut self) {
        std::mem::swap(&mut self.front, &mut self.back);
    }

    pub fn draw_back(&mut self) -> &mut CGContext {
        // Return context for back buffer
        &mut self.back_context
    }
}
```

## Error Handling

### Safe Wrapper Functions

```rust
fn safe_cgcolor_from_color(color: Color) -> core_graphics::color::CGColor {
    let r = ((color.r as f64) / 255.0).max(0.0).min(1.0);
    let g = ((color.g as f64) / 255.0).max(0.0).min(1.0);
    let b = ((color.b as f64) / 255.0).max(0.0).min(1.0);
    let a = ((color.a as f64) / 255.0).max(0.0).min(1.0);

    core_graphics::color::CGColor::rgba(r, g, b, a)
}

fn safe_rect_to_cgrect(rect: Rect) -> CGRect {
    let width = (rect.width() as f64).max(0.0);
    let height = (rect.height() as f64).max(0.0);

    CGRect::new(
        &CGPoint::new(rect.left as f64, rect.top as f64),
        &CGSize::new(width, height),
    )
}
```

## Testing Strategies

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_conversion() {
        let color = Color::new(255, 128, 64);
        let cg_color = safe_cgcolor_from_color(color);
        // Verify components
    }

    #[test]
    fn test_rect_conversion() {
        let rect = Rect::new(10, 20, 100, 200);
        let cg_rect = safe_rect_to_cgrect(rect);
        assert_eq!(cg_rect.size.width, 90.0);
        assert_eq!(cg_rect.size.height, 180.0);
    }
}
```

### Integration Tests

```rust
#[cfg(test)]
mod integration_tests {
    #[test]
    fn test_draw_to_offscreen_buffer() {
        // Create test context
        // Draw shapes
        // Verify pixel values
    }
}
```

## Memory Management

### CGContext Lifecycle

```rust
unsafe {
    // Acquire context
    let context = NSGraphicsContext::currentContext().CGContext();
    
    // Use context
    context.fill_rect(rect);
    
    // Context is automatically released when NSGraphicsContext is destroyed
}
```

### Font Cache Cleanup

```rust
pub struct FontCache {
    fonts: HashMap<String, CGFont>,
}

impl Drop for FontCache {
    fn drop(&mut self) {
        // Fonts are automatically released by CGFont's Drop impl
        self.fonts.clear();
    }
}
```

## References

- [Apple CoreGraphics API Reference](https://developer.apple.com/documentation/coregraphics)
- [Apple CoreText Programming Guide](https://developer.apple.com/library/archive/documentation/StringsTextFonts/Conceptual/CoreText_Programming/Overview/Overview.html)
- [core-graphics-rs](https://github.com/servo/core-graphics-rs)
- [core-foundation-rs](https://github.com/servo/core-foundation-rs)

## Next Steps

1. **Immediate**: Implement basic line and rectangle rendering
2. **Short-term**: Add CoreText text rendering with font support
3. **Medium-term**: Add advanced shapes and clipping
4. **Long-term**: Performance optimization and GPU acceleration
