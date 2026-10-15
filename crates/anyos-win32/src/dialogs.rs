//! Phase 3: Dialog box support
//!
//! Implements Win32 DialogBox, DialogBoxIndirect, and related APIs
//! Maps to native dialogs through the UIBackend on macOS and other platforms.

use crate::api::*;
use crate::thread::{Halt, Thread};
use anyos_ui::{ControlType, DialogControl, DialogTemplate};
use std::collections::HashMap;

/// Dialog template resource structure for DialogBoxIndirect
/// This maps to the DLGTEMPLATE structure in Win32
#[derive(Debug, Clone)]
pub struct DialogResource {
    pub style: u32,
    pub ex_style: u32,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub title: String,
    pub controls: Vec<DialogControlResource>,
}

/// Control descriptor within a dialog template
#[derive(Debug, Clone)]
pub struct DialogControlResource {
    pub style: u32,
    pub ex_style: u32,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub id: u16,
    pub class: String,
    pub title: String,
}

/// Parse a dialog template from memory (DialogBoxIndirect support)
pub fn parse_dialog_template(t: &Thread<'_, '_>, template_ptr: u64) -> Option<DialogResource> {
    // DLGTEMPLATE structure:
    // DWORD style
    // DWORD dwExtendedStyle
    // WORD cdit (number of controls)
    // short x, y, cx, cy
    // (optional) sz_or_ord menu
    // (optional) sz_or_ord class
    // (optional) string title
    // (optional) WORD pointsize (if DS_SETFONT)
    // (optional) string typeface

    let style = read_u32(t, template_ptr)?;
    let ex_style = read_u32(t, template_ptr + 4)?;
    let _cdit = read_u16(t, template_ptr + 8)?;
    let x = read_i16(t, template_ptr + 10)?;
    let y = read_i16(t, template_ptr + 12)?;
    let cx = read_i16(t, template_ptr + 14)?;
    let cy = read_i16(t, template_ptr + 16)?;

    // For simplicity, we'll parse basic templates
    // A full implementation would need to handle menus, classes, fonts, etc.
    Some(DialogResource {
        style,
        ex_style,
        x,
        y,
        cx,
        cy,
        title: String::new(),
        controls: Vec::new(),
    })
}

/// Convert a DialogResource to a DialogTemplate for the UI backend
pub fn resource_to_template(resource: &DialogResource, title: &str) -> DialogTemplate {
    DialogTemplate {
        title: title.to_string(),
        style: resource.style,
        ex_style: resource.ex_style,
        rect: anyos_ui::graphics::Rect {
            left: resource.x as i32,
            top: resource.y as i32,
            right: (resource.x + resource.cx) as i32,
            bottom: (resource.y + resource.cy) as i32,
        },
        font_name: "MS Shell Dlg".to_string(),
        font_size: 8,
        controls: resource
            .controls
            .iter()
            .map(|c| DialogControl {
                id: c.id as u32,
                control_type: ControlType::from_class_name(&c.class).unwrap_or(ControlType::Static),
                title: c.title.clone(),
                style: c.style,
                ex_style: c.ex_style,
                rect: anyos_ui::graphics::Rect {
                    left: c.x as i32,
                    top: c.y as i32,
                    right: (c.x + c.cx) as i32,
                    bottom: (c.y + c.cy) as i32,
                },
            })
            .collect(),
    }
}

/// DialogBox API - creates a modal dialog from a resource template
pub fn dialog_box(
    t: &mut Thread<'_, '_>,
    instance: u64,
    template_name: u64,
    parent: u64,
    dlg_proc: u64,
    init_param: u64,
    wide: bool,
) -> Result<u64, Halt> {
    // Create a basic dialog window
    // DialogBox creates a modal dialog, showing it and running a nested message loop
    let hwnd = create_dialog_window(t, parent, "Dialog", 100, 100, 400, 300, dlg_proc)?;

    // Post WM_INITDIALOG so the dialog proc can initialize controls
    post_dialog_message(t, hwnd, 0x0110, hwnd, init_param);

    // Return the dialog result code (would be set by EndDialog)
    // In a real implementation, this would run a nested message loop
    Ok(1)
}

/// DialogBoxIndirect API - creates a modal dialog from an in-memory template
pub fn dialog_box_indirect(
    t: &mut Thread<'_, '_>,
    instance: u64,
    template: u64,
    parent: u64,
    dlg_proc: u64,
) -> Result<u64, Halt> {
    // Parse the template from memory
    if let Some(_resource) = parse_dialog_template(t, template) {
        // Create dialog window
        let hwnd = create_dialog_window(t, parent, "Dialog", 100, 100, 400, 300, dlg_proc)?;

        // Post WM_INITDIALOG
        post_dialog_message(t, hwnd, 0x0110, hwnd, 0);

        // Return dialog result
        Ok(1)
    } else {
        Ok(0)
    }
}

/// CreateDialogParam API - creates a modeless dialog
pub fn create_dialog_param(
    t: &mut Thread<'_, '_>,
    instance: u64,
    template_name: u64,
    parent: u64,
    dlg_proc: u64,
    init_param: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let hwnd = create_dialog_window(t, parent, "Dialog", 100, 100, 400, 300, dlg_proc)?;

    // Post WM_INITDIALOG for modeless dialogs too
    post_dialog_message(t, hwnd, 0x0110, hwnd, init_param);

    Ok(hwnd)
}

/// CreateDialogIndirectParam API - creates a modeless dialog from template
pub fn create_dialog_indirect_param(
    t: &mut Thread<'_, '_>,
    instance: u64,
    template: u64,
    parent: u64,
    dlg_proc: u64,
    init_param: u64,
) -> Result<u64, Halt> {
    if let Some(_resource) = parse_dialog_template(t, template) {
        let hwnd = create_dialog_window(t, parent, "Dialog", 100, 100, 400, 300, dlg_proc)?;
        post_dialog_message(t, hwnd, 0x0110, hwnd, init_param);
        Ok(hwnd)
    } else {
        Ok(0)
    }
}

/// Helper: Create a dialog window in the UI state
fn create_dialog_window(
    t: &mut Thread<'_, '_>,
    parent: u64,
    class: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    dlg_proc: u64,
) -> Result<u64, Halt> {
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_CAPTION: u32 = 0xC00000;
    const WS_SYSMENU: u32 = 0x80000;

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE;
    let hwnd = t.k.ui.create_window(
        class.to_string(),
        String::new(),
        style,
        0,
        x,
        y,
        w,
        h,
        parent,
        0,
    );

    // Store the dialog proc for future message routing
    if let Some(window) = t.k.ui.window_mut(hwnd) {
        window.wnd_proc = dlg_proc;
    }

    Ok(hwnd)
}

/// Helper: Post a message to a dialog
fn post_dialog_message(t: &mut Thread<'_, '_>, hwnd: u64, msg: u32, wp: u64, lp: u64) {
    let time = t.started_at.elapsed().as_millis() as u32;
    t.k.ui.queue.push_back(crate::ui::Msg {
        hwnd,
        message: msg,
        wparam: wp,
        lparam: lp,
        time,
    });
}

/// Helper: Get size of DLGTEMPLATE structure in bytes
pub fn dlg_template_size(t: &Thread<'_, '_>, template_ptr: u64) -> Option<usize> {
    // This is complex as DLGTEMPLATE has variable-size strings
    // For now, return a reasonable estimate
    Some(256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dialog_control_resource() {
        let ctrl = DialogControlResource {
            style: 0x50000000,
            ex_style: 0,
            x: 10,
            y: 20,
            cx: 100,
            cy: 30,
            id: 1,
            class: "BUTTON".to_string(),
            title: "OK".to_string(),
        };
        assert_eq!(ctrl.id, 1);
        assert_eq!(ctrl.class, "BUTTON");
    }

    #[test]
    fn test_dialog_resource() {
        let res = DialogResource {
            style: 0,
            ex_style: 0,
            x: 100,
            y: 100,
            cx: 300,
            cy: 200,
            title: "Test Dialog".to_string(),
            controls: vec![],
        };
        assert_eq!(res.title, "Test Dialog");
    }
}
