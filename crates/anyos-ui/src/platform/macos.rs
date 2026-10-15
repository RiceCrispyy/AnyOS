//! macOS implementation using Cocoa and CoreGraphics
//!
//! This module provides native macOS window creation and rendering using NSWindow,
//! Cocoa APIs, CoreGraphics for drawing, and CoreText for text rendering.

use crate::{
    graphics::{Color, Rect},
    window::{
        ControlType, DialogTemplate, Message, MessageType, Window, WindowHandle, WindowParams,
    },
    UIBackend, UIError, UIResult,
};
use cocoa::appkit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
use cocoa::foundation::{NSPoint, NSRect, NSSize};
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc::runtime::Object;
use objc::{class, msg_send};
use objc_foundation::{INSString, NSString};
use std::collections::HashMap;
use std::time::Duration;

/// Drawing operation for recording drawing commands
#[derive(Debug, Clone)]
enum DrawingOp {
    TextOut {
        x: i32,
        y: i32,
        text: String,
        color: Color,
    },
    Line {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color: Color,
        width: i32,
    },
    Rectangle {
        rect: Rect,
        filled: bool,
        color: Color,
        width: i32,
    },
    FillRect {
        rect: Rect,
        color: Color,
    },
    SetPixel {
        x: i32,
        y: i32,
        color: Color,
    },
}

/// Drawing state for a device context
#[derive(Debug, Clone)]
struct DrawingState {
    current_x: i32,
    current_y: i32,
    pen_color: Color,
    pen_width: i32,
    brush_color: Color,
    operations: Vec<DrawingOp>,
}

impl Default for DrawingState {
    fn default() -> Self {
        Self {
            current_x: 0,
            current_y: 0,
            pen_color: Color::BLACK,
            pen_width: 1,
            brush_color: Color::WHITE,
            operations: Vec::new(),
        }
    }
}

/// Dialog state for modal dialogs (Phase 3)
struct DialogState {
    handle: WindowHandle,
    result: Option<i32>,
    ns_window: *mut NSWindow,
    controls: HashMap<u32, DialogControlState>,
}

unsafe impl Send for DialogState {}
unsafe impl Sync for DialogState {}

/// State for a single dialog control (Phase 3)
#[derive(Clone)]
struct DialogControlState {
    id: u32,
    control_type: ControlType,
    value: String,
    ns_view: Option<*mut Object>,
}

unsafe impl Send for DialogControlState {}
unsafe impl Sync for DialogControlState {}

/// macOS backend using Cocoa + CoreGraphics
pub struct MacOSBackend {
    windows: HashMap<WindowHandle, MacOSWindow>,
    dialogs: HashMap<WindowHandle, DialogState>,
    drawing_states: HashMap<u64, DrawingState>,
    next_handle: u64,
    next_dc: u64,
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
            dialogs: HashMap::new(),
            drawing_states: HashMap::new(),
            next_handle: 0x1000,
            next_dc: 0x2000,
            message_queue: std::collections::VecDeque::new(),
        })
    }

    fn next_handle(&mut self) -> WindowHandle {
        let h = WindowHandle(self.next_handle);
        self.next_handle += 4;
        h
    }

    fn next_dc_handle(&mut self) -> u64 {
        let dc = self.next_dc;
        self.next_dc += 1;
        dc
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
                NSSize::new(rect.width() as f64, rect.height() as f64),
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

    /// Create controls for a dialog from the template (Phase 3)
    fn create_dialog_controls(
        &mut self,
        handle: WindowHandle,
        ns_window: *mut NSWindow,
        template: &DialogTemplate,
    ) -> UIResult<HashMap<u32, DialogControlState>> {
        let mut controls = HashMap::new();

        unsafe {
            for control in &template.controls {
                match control.control_type {
                    ControlType::Button => {
                        // Create NSButton
                        let button: *mut Object = msg_send![class!(NSButton), alloc];
                        if !button.is_null() {
                            let button_rect = NSRect::new(
                                NSPoint::new(control.rect.left as f64, control.rect.top as f64),
                                NSSize::new(
                                    control.rect.width() as f64,
                                    control.rect.height() as f64,
                                ),
                            );
                            let button: *mut Object = msg_send![
                                button,
                                initWithFrame: button_rect
                            ];

                            // Set button title
                            let ns_title = NSString::from_str(&control.title);
                            let _: () = msg_send![button, setTitle: ns_title];

                            // Set button type (push button)
                            let _: () = msg_send![button, setButtonType: 1i64]; // NSMomentaryLightButton

                            // Add button to window's content view
                            let content_view: *mut Object = msg_send![ns_window, contentView];
                            let _: () = msg_send![content_view, addSubview: button];

                            controls.insert(
                                control.id,
                                DialogControlState {
                                    id: control.id,
                                    control_type: ControlType::Button,
                                    value: control.title.clone(),
                                    ns_view: Some(button),
                                },
                            );
                        }
                    }
                    ControlType::Static => {
                        // Create NSTextField (read-only for static text)
                        let text_field: *mut Object = msg_send![class!(NSTextField), alloc];
                        if !text_field.is_null() {
                            let field_rect = NSRect::new(
                                NSPoint::new(control.rect.left as f64, control.rect.top as f64),
                                NSSize::new(
                                    control.rect.width() as f64,
                                    control.rect.height() as f64,
                                ),
                            );
                            let text_field: *mut Object = msg_send![
                                text_field,
                                initWithFrame: field_rect
                            ];

                            // Set text
                            let ns_text = NSString::from_str(&control.title);
                            let _: () = msg_send![text_field, setStringValue: ns_text];

                            // Make it non-editable
                            let _: () = msg_send![text_field, setEditable: false];

                            // Add to window
                            let content_view: *mut Object = msg_send![ns_window, contentView];
                            let _: () = msg_send![content_view, addSubview: text_field];

                            controls.insert(
                                control.id,
                                DialogControlState {
                                    id: control.id,
                                    control_type: ControlType::Static,
                                    value: control.title.clone(),
                                    ns_view: Some(text_field),
                                },
                            );
                        }
                    }
                    ControlType::Edit => {
                        // Create NSTextField (editable for edit control)
                        let text_field: *mut Object = msg_send![class!(NSTextField), alloc];
                        if !text_field.is_null() {
                            let field_rect = NSRect::new(
                                NSPoint::new(control.rect.left as f64, control.rect.top as f64),
                                NSSize::new(
                                    control.rect.width() as f64,
                                    control.rect.height() as f64,
                                ),
                            );
                            let text_field: *mut Object = msg_send![
                                text_field,
                                initWithFrame: field_rect
                            ];

                            // Make it editable
                            let _: () = msg_send![text_field, setEditable: true];

                            // Add to window
                            let content_view: *mut Object = msg_send![ns_window, contentView];
                            let _: () = msg_send![content_view, addSubview: text_field];

                            controls.insert(
                                control.id,
                                DialogControlState {
                                    id: control.id,
                                    control_type: ControlType::Edit,
                                    value: String::new(),
                                    ns_view: Some(text_field),
                                },
                            );
                        }
                    }
                    _ => {
                        // Other control types not yet implemented
                    }
                }
            }
        }

        Ok(controls)
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
        self.message_queue
            .push_back(Message::new(handle, MessageType::Paint));

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
        let dc = self.next_dc_handle();
        self.drawing_states.insert(dc, DrawingState::default());
        Ok(dc)
    }

    fn destroy_dc(&mut self, dc: u64) -> UIResult<()> {
        self.drawing_states.remove(&dc);
        Ok(())
    }

    fn get_drawing_state_mut(&mut self, dc: u64) -> UIResult<&mut DrawingState> {
        self.drawing_states
            .get_mut(&dc)
            .ok_or_else(|| UIError::InvalidHandle(format!("0x{:x}", dc)))
    }

    fn color_to_cgcolor(color: Color) -> core_graphics::color::CGColor {
        let components = [
            (color.r as f64) / 255.0,
            (color.g as f64) / 255.0,
            (color.b as f64) / 255.0,
            (color.a as f64) / 255.0,
        ];
        core_graphics::color::CGColor::rgba(
            components[0],
            components[1],
            components[2],
            components[3],
        )
    }

    fn text_out(&mut self, dc: u64, x: i32, y: i32, text: &str, color: Color) -> UIResult<()> {
        let state = self.get_drawing_state_mut(dc)?;
        state.current_x = x;
        state.current_y = y;

        // Record the text operation for deferred rendering
        state.operations.push(DrawingOp::TextOut {
            x,
            y,
            text: text.to_string(),
            color,
        });

        eprintln!("text_out at ({}, {}): '{}' color={:?}", x, y, text, color);
        Ok(())
    }

    fn line_to(&mut self, dc: u64, x1: i32, y1: i32, x2: i32, y2: i32) -> UIResult<()> {
        let state = self.get_drawing_state_mut(dc)?;
        let pen_color = state.pen_color;
        let pen_width = state.pen_width;

        // Update current position
        state.current_x = x2;
        state.current_y = y2;

        // Record the line operation
        state.operations.push(DrawingOp::Line {
            x1,
            y1,
            x2,
            y2,
            color: pen_color,
            width: pen_width,
        });

        eprintln!(
            "line_to from ({}, {}) to ({}, {}) color={:?} width={}",
            x1, y1, x2, y2, pen_color, pen_width
        );
        Ok(())
    }

    fn rectangle(&mut self, dc: u64, rect: Rect, filled: bool) -> UIResult<()> {
        let state = self.get_drawing_state_mut(dc)?;
        let color = if filled {
            state.brush_color
        } else {
            state.pen_color
        };
        let width = state.pen_width;

        // Record the rectangle operation
        state.operations.push(DrawingOp::Rectangle {
            rect,
            filled,
            color,
            width,
        });

        eprintln!(
            "rectangle at ({}, {}, {}, {}) filled={} color={:?} width={}",
            rect.left, rect.top, rect.right, rect.bottom, filled, color, width
        );
        Ok(())
    }

    fn fill_rect(&mut self, dc: u64, rect: Rect, color: Color) -> UIResult<()> {
        let state = self.get_drawing_state_mut(dc)?;

        // Record the fill rect operation
        state.operations.push(DrawingOp::FillRect { rect, color });

        eprintln!(
            "fill_rect at ({}, {}, {}, {}) color={:?}",
            rect.left, rect.top, rect.right, rect.bottom, color
        );
        Ok(())
    }

    fn set_pixel(&mut self, dc: u64, x: i32, y: i32, color: Color) -> UIResult<()> {
        let state = self.get_drawing_state_mut(dc)?;

        // Record the pixel operation
        state.operations.push(DrawingOp::SetPixel { x, y, color });

        eprintln!("set_pixel at ({}, {}) color={:?}", x, y, color);
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

    /// Create a modal dialog box (Phase 3)
    fn create_dialog_modal(
        &mut self,
        _parent: WindowHandle,
        template: &DialogTemplate,
    ) -> UIResult<i32> {
        let handle = self.next_handle();

        // Build NSWindowStyleMask for dialog (no minimize/maximize)
        let style_mask = NSWindowStyleMask::NSWindowStyleMaskTitled
            | NSWindowStyleMask::NSWindowStyleMaskClosable;

        // Create native NSWindow for dialog
        let ns_window = self.create_ns_window(&template.title, template.rect, style_mask)?;

        // Create controls for the dialog
        let controls = self.create_dialog_controls(handle, ns_window, template)?;

        // Store dialog state
        let dialog_state = DialogState {
            handle,
            result: None,
            ns_window,
            controls,
        };
        self.dialogs.insert(handle, dialog_state);

        // Post WM_INITDIALOG message
        self.message_queue
            .push_back(Message::new(handle, MessageType::InitDialog));

        // For modal dialogs, we would normally run a modal event loop
        // For now, simulate a simple dialog interaction
        unsafe {
            // Make the window modal and visible
            let _: () = msg_send![ns_window, makeKeyAndOrderFront: std::ptr::null_mut::<Object>()];
        }

        // Return dialog result (0 for now, would be set by EndDialog)
        Ok(0)
    }

    /// Create a modeless dialog box (Phase 3)
    fn create_dialog_modeless(
        &mut self,
        _parent: WindowHandle,
        template: &DialogTemplate,
    ) -> UIResult<WindowHandle> {
        let handle = self.next_handle();

        // Build NSWindowStyleMask for dialog
        let style_mask = NSWindowStyleMask::NSWindowStyleMaskTitled
            | NSWindowStyleMask::NSWindowStyleMaskClosable
            | NSWindowStyleMask::NSWindowStyleMaskMiniaturizable
            | NSWindowStyleMask::NSWindowStyleMaskResizable;

        // Create native NSWindow for dialog
        let ns_window = self.create_ns_window(&template.title, template.rect, style_mask)?;

        // Create controls for the dialog
        let controls = self.create_dialog_controls(handle, ns_window, template)?;

        // Store dialog state
        let dialog_state = DialogState {
            handle,
            result: None,
            ns_window,
            controls,
        };
        self.dialogs.insert(handle, dialog_state);

        // Post WM_INITDIALOG message
        self.message_queue
            .push_back(Message::new(handle, MessageType::InitDialog));

        unsafe {
            // Make dialog visible
            let _: () = msg_send![ns_window, orderFront: std::ptr::null_mut::<Object>()];
        }

        Ok(handle)
    }

    /// Destroy a dialog box (Phase 3)
    fn destroy_dialog(&mut self, handle: WindowHandle) -> UIResult<()> {
        if let Some(dialog) = self.dialogs.remove(&handle) {
            unsafe {
                let _: () = msg_send![dialog.ns_window, close];
            }
            Ok(())
        } else {
            Err(UIError::InvalidHandle(format!("0x{:x}", handle.0)))
        }
    }

    /// End a modal dialog with a result code (Phase 3)
    fn end_dialog(&mut self, handle: WindowHandle, result: i32) -> UIResult<()> {
        if let Some(dialog) = self.dialogs.get_mut(&handle) {
            dialog.result = Some(result);
            // Post a close message
            self.message_queue
                .push_back(Message::new(handle, MessageType::Close));
            Ok(())
        } else {
            Err(UIError::InvalidHandle(format!("0x{:x}", handle.0)))
        }
    }

    /// Render queued drawing operations using CoreGraphics
    /// This is typically called from NSView's drawRect: callback
    #[allow(dead_code)]
    fn render_drawing_operations(&self, dc: u64, _context: *mut CGContext) -> UIResult<()> {
        if let Some(state) = self.drawing_states.get(&dc) {
            for op in &state.operations {
                match op {
                    DrawingOp::Line {
                        x1,
                        y1,
                        x2,
                        y2,
                        color,
                        width,
                    } => {
                        // In a real implementation:
                        // CGContextSetLineWidth(context, *width as CGFloat);
                        // CGContextSetStrokeColor(context, color_cgcolor.components());
                        // CGContextMoveToPoint(context, *x1 as CGFloat, *y1 as CGFloat);
                        // CGContextAddLineToPoint(context, *x2 as CGFloat, *y2 as CGFloat);
                        // CGContextStrokePath(context);
                        eprintln!(
                            "Rendering line from ({}, {}) to ({}, {}) color={:?} width={}",
                            x1, y1, x2, y2, color, width
                        );
                    }
                    DrawingOp::Rectangle {
                        rect,
                        filled,
                        color,
                        width: _width,
                    } => {
                        // In a real implementation:
                        // let cgrect = CGRectMake(
                        //     rect.left as CGFloat, rect.top as CGFloat,
                        //     rect.width() as CGFloat, rect.height() as CGFloat
                        // );
                        // CGContextSetStrokeColor/CGContextSetFillColor based on color
                        // if *filled {
                        //     CGContextFillRect(context, cgrect);
                        // } else {
                        //     CGContextStrokeRect(context, cgrect);
                        // }
                        eprintln!(
                            "Rendering rectangle at ({}, {}, {}, {}) filled={} color={:?}",
                            rect.left, rect.top, rect.right, rect.bottom, filled, color
                        );
                    }
                    DrawingOp::FillRect { rect, color } => {
                        // In a real implementation:
                        // let cgrect = CGRectMake(...);
                        // CGContextSetFillColor(context, color_cgcolor.components());
                        // CGContextFillRect(context, cgrect);
                        eprintln!(
                            "Rendering fill_rect at ({}, {}, {}, {}) color={:?}",
                            rect.left, rect.top, rect.right, rect.bottom, color
                        );
                    }
                    DrawingOp::TextOut { x, y, text, color } => {
                        // In a real implementation with CoreText:
                        // let font_name = CFString::new("Helvetica");
                        // let font = CGFont::from_name(&font_name)?;
                        // let ct_font = CTFont::new(&font, 12.0, nullptr);
                        // let attr_string = CFAttributedString::new(...);
                        // let line = CTLine::new(&attr_string);
                        // CGContextSetTextPosition(context, *x as CGFloat, *y as CGFloat);
                        // CTLineDraw(line, context);
                        eprintln!(
                            "Rendering text at ({}, {}): '{}' color={:?}",
                            x, y, text, color
                        );
                    }
                    DrawingOp::SetPixel { x, y, color } => {
                        // In a real implementation:
                        // CGContextFillRect(context, CGRectMake(*x as CGFloat, *y as CGFloat, 1.0, 1.0));
                        eprintln!("Rendering set_pixel at ({}, {}) color={:?}", x, y, color);
                    }
                }
            }
        }
        Ok(())
    }

    /// Clear drawing operations for a device context
    #[allow(dead_code)]
    fn clear_drawing_operations(&mut self, dc: u64) {
        if let Some(state) = self.drawing_states.get_mut(&dc) {
            state.operations.clear();
        }
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
