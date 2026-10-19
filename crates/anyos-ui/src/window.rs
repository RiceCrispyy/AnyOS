//! Window management abstractions

use crate::graphics::Rect;

/// Opaque window handle
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct WindowHandle(pub u64);

impl WindowHandle {
    pub fn from_u64(val: u64) -> Self {
        WindowHandle(val)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Window creation parameters
#[derive(Debug, Clone)]
pub struct WindowParams {
    /// Parent window (0 for top-level)
    pub parent: WindowHandle,

    /// Window class name
    pub class: String,

    /// Window title
    pub title: String,

    /// Window style flags (WS_* from Windows)
    pub style: u32,

    /// Extended window style flags (WS_EX_* from Windows)
    pub ex_style: u32,

    /// Position and size
    pub rect: Rect,

    /// Window instance handle (for icons, etc.)
    pub instance: u64,

    /// User-defined data
    pub user_data: u64,

    /// Window menu (if any)
    pub menu: u64,

    /// Control ID (for child windows)
    pub id: u32,
}

impl Default for WindowParams {
    fn default() -> Self {
        Self {
            parent: WindowHandle(0),
            class: "STATIC".to_string(),
            title: String::new(),
            style: 0,
            ex_style: 0,
            rect: Rect::default(),
            instance: 0,
            user_data: 0,
            menu: 0,
            id: 0,
        }
    }
}

/// Window state
#[derive(Debug, Clone)]
pub struct Window {
    pub handle: WindowHandle,
    pub class: String,
    pub title: String,
    pub visible: bool,
    pub enabled: bool,
    pub parent: WindowHandle,
}

/// Dialog box control types (Phase 3)
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum ControlType {
    Button,
    Static,
    Edit,
    ListBox,
    ComboBox,
    ScrollBar,
}

impl ControlType {
    pub fn from_class_name(name: &str) -> Option<Self> {
        match name {
            "BUTTON" => Some(ControlType::Button),
            "STATIC" => Some(ControlType::Static),
            "EDIT" => Some(ControlType::Edit),
            "LISTBOX" => Some(ControlType::ListBox),
            "COMBOBOX" => Some(ControlType::ComboBox),
            "SCROLLBAR" => Some(ControlType::ScrollBar),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            ControlType::Button => "BUTTON",
            ControlType::Static => "STATIC",
            ControlType::Edit => "EDIT",
            ControlType::ListBox => "LISTBOX",
            ControlType::ComboBox => "COMBOBOX",
            ControlType::ScrollBar => "SCROLLBAR",
        }
    }
}

/// Dialog control descriptor
#[derive(Debug, Clone)]
pub struct DialogControl {
    pub id: u32,
    pub control_type: ControlType,
    pub title: String,
    pub style: u32,
    pub ex_style: u32,
    pub rect: Rect,
}

/// Dialog template for creating dialogs with controls
#[derive(Debug, Clone)]
pub struct DialogTemplate {
    pub title: String,
    pub style: u32,
    pub ex_style: u32,
    pub rect: Rect,
    pub font_name: String,
    pub font_size: u16,
    pub controls: Vec<DialogControl>,
}

impl Default for DialogTemplate {
    fn default() -> Self {
        Self {
            title: String::new(),
            style: WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            ex_style: 0,
            rect: Rect {
                left: 100,
                top: 100,
                right: 500,
                bottom: 400,
            },
            font_name: "MS Shell Dlg".to_string(),
            font_size: 8,
            controls: Vec::new(),
        }
    }
}

/// Message types
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MessageType {
    Quit,
    Paint,
    KeyDown,
    KeyUp,
    Char,
    MouseMove,
    LeftButtonDown,
    LeftButtonUp,
    RightButtonDown,
    RightButtonUp,
    MiddleButtonDown,
    MiddleButtonUp,
    InitDialog,
    Command,
    Close,
    Other(u32),
}

/// Window message
#[derive(Debug, Clone)]
pub struct Message {
    pub hwnd: WindowHandle,
    pub msg_type: MessageType,
    pub wparam: u64,
    pub lparam: u64,
    pub time: u32,
    pub x: i32,
    pub y: i32,
}

impl Message {
    pub fn new(hwnd: WindowHandle, msg_type: MessageType) -> Self {
        Self {
            hwnd,
            msg_type,
            wparam: 0,
            lparam: 0,
            time: 0,
            x: 0,
            y: 0,
        }
    }

    pub fn with_params(mut self, wparam: u64, lparam: u64) -> Self {
        self.wparam = wparam;
        self.lparam = lparam;
        self
    }

    pub fn with_position(mut self, x: i32, y: i32) -> Self {
        self.x = x;
        self.y = y;
        self
    }
}

// Common Windows style constants
pub mod styles {
    /// Overlapped window (has title bar, border, minimize/maximize buttons)
    pub const WS_OVERLAPPEDWINDOW: u32 = 0xCF0000;

    /// Window is visible
    pub const WS_VISIBLE: u32 = 0x10000000;

    /// Window is child window
    pub const WS_CHILD: u32 = 0x40000000;

    /// Window is popup
    pub const WS_POPUP: u32 = 0x80000000;

    /// Window has minimize button
    pub const WS_MINIMIZEBOX: u32 = 0x20000;

    /// Window has maximize button
    pub const WS_MAXIMIZEBOX: u32 = 0x10000;

    /// Window has system menu
    pub const WS_SYSMENU: u32 = 0x80000;

    /// Window has caption
    pub const WS_CAPTION: u32 = 0xC00000;

    /// Window has border
    pub const WS_BORDER: u32 = 0x800000;
}

// Re-export window style constants at module level for convenience
pub use styles::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_handle() {
        let handle = WindowHandle::from_u64(42);
        assert_eq!(handle.as_u64(), 42);
    }

    #[test]
    fn test_message_construction() {
        let msg = Message::new(WindowHandle(1), MessageType::Quit).with_params(0, 0);

        assert_eq!(msg.hwnd, WindowHandle(1));
        assert_eq!(msg.msg_type, MessageType::Quit);
    }
}
