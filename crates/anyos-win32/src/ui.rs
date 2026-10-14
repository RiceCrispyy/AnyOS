//! Stub implementations of user32, gdi32, comctl32, comdlg32 and shell32 APIs.
//!
//! None of these render real pixels. The goal is to keep the message loop
//! alive long enough for an Inno Setup installer to reach the file-copying
//! stage. Every HWND, HDC, HFONT, HBRUSH etc. is a fake handle allocated
//! from a monotonic counter.

use crate::api::*;
use crate::thread::{Halt, Thread};

// ---------------------------------------------------------------------------
// Fake handle allocation

/// Base value for fake GDI/USER handles so they don't collide with the
/// Windows kernel handle table (which starts at 0x100).
const UI_BASE: u64 = 0x4000_0000;

/// State the UI stubs share, kept inside `Kernel`.
#[derive(Default)]
pub(crate) struct UiState {
    next: u64,
    pub windows: Vec<Window>,
    /// Posted/sent messages waiting for GetMessage / PeekMessage.
    pub queue: std::collections::VecDeque<Msg>,
    /// WM_QUIT code (set by PostQuitMessage).
    pub quit: Option<u32>,
    /// Class registrations (we just remember the WndProc).
    pub classes: Vec<WndClass>,
}

pub(crate) struct Window {
    pub hwnd: u64,
    pub parent: u64,
    pub class: String,
    pub title: String,
    pub wnd_proc: u64,
    pub visible: bool,
    pub enabled: bool,
    pub style: u32,
    pub ex_style: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Dialog result from EndDialog.
    pub dialog_result: Option<i32>,
    pub user_data: u64,
    pub id: u64,
}

#[derive(Clone)]
pub(crate) struct Msg {
    pub hwnd: u64,
    pub message: u32,
    pub wparam: u64,
    pub lparam: u64,
    pub time: u32,
}

pub(crate) struct WndClass {
    pub name: String,
    pub wnd_proc: u64,
    pub atom: u16,
}

impl UiState {
    fn next_handle(&mut self) -> u64 {
        if self.next == 0 {
            self.next = UI_BASE;
        }
        let h = self.next;
        self.next += 4;
        h
    }

    pub fn create_window(
        &mut self,
        class: String,
        title: String,
        style: u32,
        ex_style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: u64,
        id: u64,
    ) -> u64 {
        let hwnd = self.next_handle();
        let wnd_proc = self
            .classes
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&class))
            .map(|c| c.wnd_proc)
            .unwrap_or(0);
        self.windows.push(Window {
            hwnd,
            parent,
            class,
            title,
            wnd_proc,
            visible: (style & WS_VISIBLE) != 0,
            enabled: true,
            style,
            ex_style,
            x,
            y,
            w,
            h,
            dialog_result: None,
            user_data: 0,
            id,
        });
        hwnd
    }

    pub fn window(&self, hwnd: u64) -> Option<&Window> {
        self.windows.iter().find(|w| w.hwnd == hwnd)
    }

    pub fn window_mut(&mut self, hwnd: u64) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.hwnd == hwnd)
    }

    pub fn next_gdi(&mut self) -> u64 {
        self.next_handle()
    }
}

// ---------------------------------------------------------------------------
// Window style constants

const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_DISABLED: u32 = 0x0800_0000;

// ---------------------------------------------------------------------------
// Common dialog messages

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_MOVE: u32 = 0x0003;
const WM_SIZE: u32 = 0x0005;
const WM_SETTEXT: u32 = 0x000C;
const WM_GETTEXT: u32 = 0x000D;
const WM_GETTEXTLENGTH: u32 = 0x000E;
const WM_PAINT: u32 = 0x000F;
const WM_QUIT: u32 = 0x0012;
const WM_INITDIALOG: u32 = 0x0110;
const WM_COMMAND: u32 = 0x0111;
const WM_NOTIFY: u32 = 0x004E;
const WM_SETFONT: u32 = 0x0030;
const WM_GETFONT: u32 = 0x0031;
const WM_NCCREATE: u32 = 0x0081;
const WM_NCDESTROY: u32 = 0x0082;

// ---------------------------------------------------------------------------
// Helper: call a WndProc on the guest if it's a real address

/// Invoke a guest WndProc stub-free: just post the message on the queue.
fn post(t: &mut Thread<'_, '_>, hwnd: u64, msg: u32, wp: u64, lp: u64) {
    let time = t.started_at.elapsed().as_millis() as u32;
    t.k.ui.queue.push_back(Msg {
        hwnd,
        message: msg,
        wparam: wp,
        lparam: lp,
        time,
    });
}

// ---------------------------------------------------------------------------
// MSG structure layout (flat, used for GetMessage / PeekMessage)
// HWND hwnd; UINT message; WPARAM wParam; LPARAM lParam; DWORD time; POINT pt;

fn write_msg(t: &mut Thread<'_, '_>, ptr: u64, m: &Msg) -> bool {
    let long = t.long_mode();
    if long {
        write_u64(t, ptr, m.hwnd)
            && write_u32(t, ptr + 8, m.message)
            && write_u64(t, ptr + 16, m.wparam)
            && write_u64(t, ptr + 24, m.lparam)
            && write_u32(t, ptr + 32, m.time)
    } else {
        write_u32(t, ptr, m.hwnd as u32)
            && write_u32(t, ptr + 4, m.message)
            && write_u32(t, ptr + 8, m.wparam as u32)
            && write_u32(t, ptr + 12, m.lparam as u32)
            && write_u32(t, ptr + 16, m.time)
    }
}

// ---------------------------------------------------------------------------
// Window management

fn register_class(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    // WNDCLASSEX: cbSize(4) style(4) lpfnWndProc(ptr) cbClsExtra(4)
    // cbWndExtra(4) hInstance(ptr) ... lpszClassName(ptr)
    let base = a[0];
    let p = t.ptr_size() as u64;
    let wnd_proc = read_ptr(t, base + 4 + p - p + 4).unwrap_or(0); // offset 8 on x86
                                                                   // On x86: cbSize(4), style(4), wndProc(4) = offset 8
                                                                   // On x64: cbSize(4), style(4), pad(4?), wndProc(8) = offset 8 or 16
    let wnd_proc = if t.long_mode() {
        read_u64(t, base + 16).unwrap_or(0) // cbSize(4)+style(4)+pad(8) = 16? No
    } else {
        read_u32(t, base + 8).map(u64::from).unwrap_or(0)
    };
    // WNDCLASSEX x86: cbSize(4),style(4),lpfnWndProc(4),cbClsExtra(4),cbWndExtra(4),
    //   hInstance(4),hIcon(4),hCursor(4),hbrBackground(4),lpszMenuName(4),lpszClassName(4)
    // WNDCLASSEX x64: cbSize(4),style(4),lpfnWndProc(8),cbClsExtra(4),cbWndExtra(4),
    //   hInstance(8),hIcon(8),hCursor(8),hbrBackground(8),lpszMenuName(8),lpszClassName(8)
    let (wnd_proc, name_ptr) = if t.long_mode() {
        let wnd_proc = read_u64(t, base + 8).unwrap_or(0);
        let name_ptr = read_u64(t, base + 64).unwrap_or(0);
        (wnd_proc, name_ptr)
    } else {
        let wnd_proc = read_u32(t, base + 8).map(u64::from).unwrap_or(0);
        let name_ptr = read_u32(t, base + 40).map(u64::from).unwrap_or(0);
        (wnd_proc, name_ptr)
    };
    let name = if name_ptr < 0xC000 {
        format!("#{}", name_ptr)
    } else {
        read_str(t, name_ptr, wide).unwrap_or_default()
    };
    let atom = (0xC001u16).wrapping_add(t.k.ui.classes.len() as u16);
    t.k.ui.classes.push(WndClass {
        name,
        wnd_proc,
        atom,
    });
    Ok(atom as u64)
}

fn create_window_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (ex_style, class_ptr, title_ptr, style) = (a[0] as u32, a[1], a[2], a[3] as u32);
    let (x, y, w, h) = (a[4] as i32, a[5] as i32, a[6] as i32, a[7] as i32);
    let (parent, id) = (a[8], a[9]);
    let class = if class_ptr < 0xC000 {
        format!("#{}", class_ptr)
    } else {
        read_str(t, class_ptr, wide).unwrap_or_default()
    };
    let title = if title_ptr == 0 {
        String::new()
    } else {
        read_str(t, title_ptr, wide).unwrap_or_default()
    };
    let hwnd =
        t.k.ui
            .create_window(class, title, style, ex_style, x, y, w, h, parent, id);
    Ok(hwnd)
}

fn destroy_window(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let hwnd = a[0];
    t.k.ui.windows.retain(|w| w.hwnd != hwnd);
    Ok(1)
}

fn show_window(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, cmd) = (a[0], a[1]);
    if let Some(w) = t.k.ui.window_mut(hwnd) {
        let was_visible = w.visible;
        w.visible = cmd != 0; // SW_HIDE = 0
        return Ok(was_visible as u64);
    }
    Ok(0)
}

fn get_window_text(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, buf, cch) = (a[0], a[1], a[2] as u32);
    let title =
        t.k.ui
            .window(hwnd)
            .map(|w| w.title.clone())
            .unwrap_or_default();
    let (n, _) = copy_str_truncate(t, buf, cch as u64, &title, wide);
    Ok(n)
}

fn set_window_text(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, ptr) = (a[0], a[1]);
    let text = read_str(t, ptr, wide).unwrap_or_default();
    if let Some(w) = t.k.ui.window_mut(hwnd) {
        w.title = text;
    }
    Ok(1)
}

fn set_window_pos(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let hwnd = a[0];
    let (x, y, cx, cy) = (a[2] as i32, a[3] as i32, a[4] as i32, a[5] as i32);
    let flags = a[6] as u32;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOSIZE: u32 = 0x0001;
    if let Some(w) = t.k.ui.window_mut(hwnd) {
        if flags & SWP_NOMOVE == 0 {
            w.x = x;
            w.y = y;
        }
        if flags & SWP_NOSIZE == 0 {
            w.w = cx;
            w.h = cy;
        }
    }
    Ok(1)
}

fn get_window_rect(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, rect_ptr) = (a[0], a[1]);
    let (x, y, w, h) =
        t.k.ui
            .window(hwnd)
            .map(|w| (w.x, w.y, w.w, w.h))
            .unwrap_or((0, 0, 640, 480));
    write_u32(t, rect_ptr, x as u32);
    write_u32(t, rect_ptr + 4, y as u32);
    write_u32(t, rect_ptr + 8, (x + w) as u32);
    write_u32(t, rect_ptr + 12, (y + h) as u32);
    Ok(1)
}

fn get_client_rect(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, rect_ptr) = (a[0], a[1]);
    let (w, h) =
        t.k.ui
            .window(hwnd)
            .map(|w| (w.w, w.h))
            .unwrap_or((640, 480));
    write_u32(t, rect_ptr, 0);
    write_u32(t, rect_ptr + 4, 0);
    write_u32(t, rect_ptr + 8, w as u32);
    write_u32(t, rect_ptr + 12, h as u32);
    Ok(1)
}

fn enable_window(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, enable) = (a[0], a[1]);
    if let Some(w) = t.k.ui.window_mut(hwnd) {
        let was_disabled = !w.enabled;
        w.enabled = enable != 0;
        return Ok(was_disabled as u64);
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Message loop

fn peek_get_message(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    remove: bool,
    blocking: bool,
) -> Result<u64, Halt> {
    let msg_ptr = a[0];
    // Check for WM_QUIT first
    if let Some(code) = t.k.ui.quit.take() {
        let m = Msg {
            hwnd: 0,
            message: WM_QUIT,
            wparam: code as u64,
            lparam: 0,
            time: t.started_at.elapsed().as_millis() as u32,
        };
        write_msg(t, msg_ptr, &m);
        // Return 0 for GetMessage on WM_QUIT, 1 for PeekMessage
        return Ok(if blocking { 0 } else { 1 });
    }
    if let Some(m) = if remove {
        t.k.ui.queue.pop_front()
    } else {
        t.k.ui.queue.front().cloned()
    } {
        write_msg(t, msg_ptr, &m);
        return Ok(1);
    }
    if blocking {
        // No messages: synthesize a WM_PAINT for the first visible window
        // so the app's paint loop runs, then return 1 (message available).
        let hwnd = t.k.ui.windows.iter().find(|w| w.visible).map(|w| w.hwnd);
        if let Some(hwnd) = hwnd {
            let m = Msg {
                hwnd,
                message: WM_PAINT,
                wparam: 0,
                lparam: 0,
                time: t.started_at.elapsed().as_millis() as u32,
            };
            write_msg(t, msg_ptr, &m);
            return Ok(1);
        }
        // No windows: quit
        let m = Msg {
            hwnd: 0,
            message: WM_QUIT,
            wparam: 0,
            lparam: 0,
            time: t.started_at.elapsed().as_millis() as u32,
        };
        write_msg(t, msg_ptr, &m);
        Ok(0)
    } else {
        // PeekMessage with no messages: write zeros and return 0
        write_msg(
            t,
            msg_ptr,
            &Msg {
                hwnd: 0,
                message: 0,
                wparam: 0,
                lparam: 0,
                time: 0,
            },
        );
        Ok(0)
    }
}

fn dispatch_message(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // We don't actually call back into guest WndProcs to avoid re-entrant
    // complexity. Any guest that inspects message loop return values gets 0.
    let _ = a;
    Ok(0)
}

fn send_message(t: &mut Thread<'_, '_>, a: &[u64], _wide: bool) -> Result<u64, Halt> {
    let (hwnd, msg, wp, lp) = (a[0], a[1] as u32, a[2], a[3]);
    // Handle the messages we can answer without a real WndProc.
    match msg {
        WM_GETTEXT => {
            let buf = wp as u64;
            let cch = lp as u32;
            let title =
                t.k.ui
                    .window(hwnd)
                    .map(|w| w.title.clone())
                    .unwrap_or_default();
            let (n, _) = copy_str_truncate(t, buf, cch as u64, &title, false);
            return Ok(n);
        }
        WM_GETTEXTLENGTH => {
            let len =
                t.k.ui
                    .window(hwnd)
                    .map(|w| w.title.len() as u64)
                    .unwrap_or(0);
            return Ok(len);
        }
        WM_SETTEXT => {
            let text = read_ansi(t, lp).unwrap_or_default();
            if let Some(w) = t.k.ui.window_mut(hwnd) {
                w.title = text;
            }
            return Ok(1);
        }
        _ => {}
    }
    Ok(0)
}

fn post_message(t: &mut Thread<'_, '_>, a: &[u64], _wide: bool) -> Result<u64, Halt> {
    let (hwnd, msg, wp, lp) = (a[0], a[1] as u32, a[2], a[3]);
    if msg == WM_QUIT {
        t.k.ui.quit = Some(wp as u32);
    } else {
        post(t, hwnd, msg, wp, lp);
    }
    Ok(1)
}

fn post_quit_message(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    t.k.ui.quit = Some(a[0] as u32);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Dialogs

fn message_box(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let text_ptr = a[1];
    let caption_ptr = a[2];
    let _flags = a[3];
    let text = read_str(t, text_ptr, wide).unwrap_or_default();
    let caption = read_str(t, caption_ptr, wide).unwrap_or_default();
    let _ = writeln!(t.stdio.stdout, "[MessageBox] {}: {}", caption, text);
    // IDOK = 1
    Ok(1)
}

fn dialog_box_param(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    // Create a fake dialog window, call initproc, run the message loop until
    // EndDialog is called.
    let (_hinstance, _template, parent, dlg_proc, init_param) = (a[0], a[1], a[2], a[3], a[4]);
    if dlg_proc == 0 {
        return Ok(0);
    }
    let hwnd = t.k.ui.create_window(
        "Dialog".into(),
        String::new(),
        WS_VISIBLE,
        0,
        100,
        100,
        400,
        300,
        parent,
        0,
    );
    // Post WM_INITDIALOG so the dialog proc runs on the next DispatchMessage.
    post(t, hwnd, WM_INITDIALOG, hwnd, init_param);
    // Return the dialog result (EndDialog stores it).
    // Since we don't actually pump a nested message loop here, just return 1.
    Ok(1)
}

fn end_dialog(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, result) = (a[0], a[1] as i32);
    if let Some(w) = t.k.ui.window_mut(hwnd) {
        w.dialog_result = Some(result);
    }
    t.k.ui.windows.retain(|w| w.hwnd != hwnd);
    Ok(1)
}

fn get_dlg_item(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, id) = (a[0], a[1] as u64);
    let found =
        t.k.ui
            .windows
            .iter()
            .find(|w| w.parent == hwnd && w.id == id)
            .map(|w| w.hwnd)
            .unwrap_or(0);
    Ok(found)
}

fn set_dlg_item_text(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, id, ptr) = (a[0], a[1] as u64, a[2]);
    let text = read_str(t, ptr, wide).unwrap_or_default();
    let child_hwnd =
        t.k.ui
            .windows
            .iter()
            .find(|w| w.parent == hwnd && w.id == id)
            .map(|w| w.hwnd);
    if let Some(ch) = child_hwnd {
        if let Some(w) = t.k.ui.window_mut(ch) {
            w.title = text;
        }
    }
    Ok(1)
}

fn get_dlg_item_text(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, id, buf, cch) = (a[0], a[1] as u64, a[2], a[3] as u32);
    let child_hwnd =
        t.k.ui
            .windows
            .iter()
            .find(|w| w.parent == hwnd && w.id == id)
            .map(|w| w.hwnd);
    let title = child_hwnd
        .and_then(|h| t.k.ui.window(h))
        .map(|w| w.title.clone())
        .unwrap_or_default();
    let (n, _) = copy_str_truncate(t, buf, cch as u64, &title, wide);
    Ok(n)
}

// ---------------------------------------------------------------------------
// DC and painting

fn get_dc(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn create_compatible_dc(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn delete_dc(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn begin_paint(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // PAINTSTRUCT: hdc(ptr) fErase(4) rcPaint(16) ...
    let paint_ptr = a[1];
    let hdc = t.k.ui.next_gdi();
    write_ptr(t, paint_ptr, hdc);
    write_u32(t, paint_ptr + t.ptr_size() as u64, 0); // fErase
    Ok(hdc)
}

fn end_paint(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

// ---------------------------------------------------------------------------
// System metrics / parameters

const SM_CXSCREEN: u32 = 0;
const SM_CYSCREEN: u32 = 1;
const SM_CXFULLSCREEN: u32 = 16;
const SM_CYFULLSCREEN: u32 = 17;
const SM_CXICON: u32 = 11;
const SM_CYICON: u32 = 12;
const SM_CYMENU: u32 = 15;
const SM_CYCAPTION: u32 = 4;

fn get_system_metrics(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let v = match a[0] as u32 {
        SM_CXSCREEN | SM_CXFULLSCREEN => 1920,
        SM_CYSCREEN | SM_CYFULLSCREEN => 1080,
        SM_CXICON | SM_CYICON => 32,
        SM_CYMENU => 19,
        SM_CYCAPTION => 23,
        _ => 0,
    };
    Ok(v)
}

fn system_parameters_info(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let _ = a;
    Ok(1)
}

// ---------------------------------------------------------------------------
// Focus / input

fn get_focus(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.windows.first().map(|w| w.hwnd).unwrap_or(0))
}

fn set_focus(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Return the previously focused window (we just return the same).
    let _ = t;
    Ok(a[0])
}

fn set_capture(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(a[0])
}

fn release_capture(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

// ---------------------------------------------------------------------------
// Windows properties

fn get_window_long(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, idx) = (a[0], a[1] as i32);
    let _ = wide;
    match idx {
        -16 => Ok(t.k.ui.window(hwnd).map(|w| w.style as u64).unwrap_or(0)), // GWL_STYLE
        -20 => Ok(t.k.ui.window(hwnd).map(|w| w.ex_style as u64).unwrap_or(0)), // GWL_EXSTYLE
        -21 => Ok(t.k.ui.window(hwnd).map(|w| w.user_data).unwrap_or(0)),    // GWL_USERDATA
        _ => Ok(0),
    }
}

fn set_window_long(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, idx, val) = (a[0], a[1] as i32, a[2]);
    let _ = wide;
    if let Some(w) = t.k.ui.window_mut(hwnd) {
        let old = match idx {
            -16 => {
                let old = w.style;
                w.style = val as u32;
                old as u64
            }
            -20 => {
                let old = w.ex_style;
                w.ex_style = val as u32;
                old as u64
            }
            -21 => {
                let old = w.user_data;
                w.user_data = val;
                old
            }
            _ => 0,
        };
        return Ok(old);
    }
    Ok(0)
}

fn is_window_visible(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.window(a[0]).map(|w| w.visible as u64).unwrap_or(0))
}

fn is_window_enabled(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.window(a[0]).map(|w| w.enabled as u64).unwrap_or(0))
}

fn is_window(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.window(a[0]).is_some() as u64)
}

// ---------------------------------------------------------------------------
// Accelerators / cursors / icons

fn load_cursor(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(UI_BASE + 1)
}

fn load_icon(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(UI_BASE + 2)
}

fn load_image(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn load_bitmap(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn set_cursor(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(a[0]) // return previous cursor
}

fn load_accelerators(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(UI_BASE + 3)
}

fn translate_accelerator(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0) // no accelerator
}

// ---------------------------------------------------------------------------
// GDI stubs

fn create_font(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn delete_object(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_stock_object(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Return a non-zero fake handle; index it so callers can tell them apart.
    Ok(UI_BASE + 0x100 + a[0])
}

fn select_object(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn create_solid_brush(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn create_pen(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn create_compatible_bitmap(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn set_bk_mode(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1) // OPAQUE
}

fn set_text_color(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(a[0]) // previous color
}

fn set_bk_color(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(a[0])
}

fn get_text_metrics(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Fill a TEXTMETRIC with plausible values so text layout doesn't divide by zero.
    let ptr = a[1];
    let _ = _t;
    if ptr != 0 {
        // tmHeight=16, tmAscent=13, tmDescent=3, tmInternalLeading=1, tmExternalLeading=2 ...
        // Just zero the structure and set height.
        // TEXTMETRICA is 60 bytes; TEXTMETRICW is 60 bytes.
        // struct: tmHeight(4),tmAscent(4),tmDescent(4),tmInternalLeading(4),tmExternalLeading(4),...
        write_u32(_t, ptr, 16); // tmHeight
        write_u32(_t, ptr + 4, 13); // tmAscent
        write_u32(_t, ptr + 8, 3); // tmDescent
        write_u32(_t, ptr + 12, 1); // tmInternalLeading
        write_u32(_t, ptr + 16, 2); // tmExternalLeading
        write_u32(_t, ptr + 20, 8); // tmAveCharWidth
        write_u32(_t, ptr + 24, 16); // tmMaxCharWidth
        write_u32(_t, ptr + 28, 0); // tmWeight
        write_u32(_t, ptr + 32, 0); // tmOverhang
        write_u32(_t, ptr + 36, 96); // tmDigitizedAspectX
        write_u32(_t, ptr + 40, 96); // tmDigitizedAspectY
    }
    Ok(1)
}

fn get_text_extent_point(_t: &mut Thread<'_, '_>, a: &[u64], _wide: bool) -> Result<u64, Halt> {
    // Fill SIZE: cx=chars*8, cy=16
    let ptr = a[4];
    let len = a[3] as u32;
    if ptr != 0 {
        write_u32(_t, ptr, len.saturating_mul(8));
        write_u32(_t, ptr + 4, 16);
    }
    Ok(1)
}

fn bit_blt(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn fill_rect(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn draw_text(_t: &mut Thread<'_, '_>, _a: &[u64], _wide: bool) -> Result<u64, Halt> {
    Ok(16) // return line height
}

fn text_out(_t: &mut Thread<'_, '_>, _a: &[u64], _wide: bool) -> Result<u64, Halt> {
    Ok(1)
}

fn ext_text_out(_t: &mut Thread<'_, '_>, _a: &[u64], _wide: bool) -> Result<u64, Halt> {
    Ok(1)
}

fn rectangle(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn ellipse(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn line_to(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn move_to_ex(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn set_stretch_blt_mode(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn stretch_blt(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_device_caps(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Return plausible values for the screen DC.
    let index = a[1] as i32;
    let v = match index {
        2 => 1920,  // HORZRES
        4 => 1080,  // VERTRES
        88 => 1920, // DESKTOPHORZRES
        90 => 1080, // DESKTOPVERTRES
        12 => 96,   // LOGPIXELSX
        14 => 96,   // LOGPIXELSY
        8 => 508,   // HORZSIZE mm
        10 => 286,  // VERTSIZE mm
        28 => 32,   // BITSPIXEL
        _ => 0,
    };
    Ok(v)
}

fn get_sys_color(_t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let _ = a;
    Ok(0x00D4D0C8) // light gray
}

fn get_sys_color_brush(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn invalidate_rect(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn update_window(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn redraw_window(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_foreground_window(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.windows.first().map(|w| w.hwnd).unwrap_or(0))
}

fn set_foreground_window(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn find_window(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let class_ptr = a[0];
    let title_ptr = a[1];
    let class = if class_ptr != 0 {
        read_str(t, class_ptr, wide)
    } else {
        None
    };
    let title = if title_ptr != 0 {
        read_str(t, title_ptr, wide)
    } else {
        None
    };
    for w in &t.k.ui.windows {
        let class_match = class
            .as_ref()
            .map_or(true, |c| w.class.eq_ignore_ascii_case(c));
        let title_match = title.as_ref().map_or(true, |t| w.title == *t);
        if class_match && title_match {
            return Ok(w.hwnd);
        }
    }
    Ok(0)
}

fn get_window_thread_process_id(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let pid_ptr = a[1];
    if pid_ptr != 0 {
        write_u32(t, pid_ptr, crate::thread::PROCESS_ID as u32);
    }
    Ok(t.thread_id() as u64)
}

fn def_window_proc(_t: &mut Thread<'_, '_>, a: &[u64], _wide: bool) -> Result<u64, Halt> {
    let msg = a[1] as u32;
    match msg {
        WM_NCCREATE => Ok(1),
        WM_CREATE => Ok(0),
        WM_DESTROY => Ok(0),
        WM_NCDESTROY => Ok(0),
        _ => Ok(0),
    }
}

fn call_window_proc(_t: &mut Thread<'_, '_>, _a: &[u64], _wide: bool) -> Result<u64, Halt> {
    Ok(0)
}

fn send_dlg_item_message(t: &mut Thread<'_, '_>, a: &[u64], _wide: bool) -> Result<u64, Halt> {
    let (hwnd, id, msg, wp, lp) = (a[0], a[1] as u64, a[2] as u32, a[3], a[4]);
    let child_hwnd =
        t.k.ui
            .windows
            .iter()
            .find(|w| w.parent == hwnd && w.id == id)
            .map(|w| w.hwnd)
            .unwrap_or(0);
    send_message(t, &[child_hwnd, msg as u64, wp, lp], false)
}

fn check_dlg_button(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, id, check) = (a[0], a[1] as u64, a[2]);
    let child_hwnd =
        t.k.ui
            .windows
            .iter()
            .find(|w| w.parent == hwnd && w.id == id)
            .map(|w| w.hwnd)
            .unwrap_or(0);
    // Store check state in user_data
    if let Some(w) = t.k.ui.window_mut(child_hwnd) {
        w.user_data = check;
    }
    Ok(1)
}

fn is_dlg_button_checked(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, id) = (a[0], a[1] as u64);
    let child_hwnd =
        t.k.ui
            .windows
            .iter()
            .find(|w| w.parent == hwnd && w.id == id)
            .map(|w| w.hwnd)
            .unwrap_or(0);
    Ok(t.k.ui.window(child_hwnd).map(|w| w.user_data).unwrap_or(0))
}

fn set_timer(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn kill_timer(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_message_pos(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn get_message_time(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn wait_message(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_parent(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.window(a[0]).map(|w| w.parent).unwrap_or(0))
}

fn get_window(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (hwnd, cmd) = (a[0], a[1] as u32);
    // GW_HWNDNEXT=2, GW_HWNDPREV=3, GW_OWNER=4, GW_CHILD=5
    match cmd {
        4 => Ok(t.k.ui.window(hwnd).map(|w| w.parent).unwrap_or(0)),
        _ => Ok(0),
    }
}

fn enum_windows(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn enum_child_windows(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn screen_to_client(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn client_to_screen(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn map_window_points(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn adjust_window_rect(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_cursor_pos(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let ptr = a[0];
    write_u32(t, ptr, 0);
    write_u32(t, ptr + 4, 0);
    Ok(1)
}

fn get_async_key_state(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0) // key not pressed
}

fn get_key_state(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn translate_message(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn register_window_message(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    // Return a unique message ID starting above WM_USER (0x8000+)
    let _ = read_str(t, a[0], wide);
    Ok(0xC000 + t.k.ui.classes.len() as u64)
}

fn copy_rect(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (dst, src) = (a[0], a[1]);
    if let (Some(b), _) = (crate::api::guest_bytes(t, src, 16), ()) {
        write_bytes(t, dst, &b);
    }
    Ok(1)
}

fn inflate_rect(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (ptr, dx, dy) = (a[0], a[1] as i32, a[2] as i32);
    if let (Some(l), Some(t_val), Some(r), Some(b)) = (
        read_u32(t, ptr),
        read_u32(t, ptr + 4),
        read_u32(t, ptr + 8),
        read_u32(t, ptr + 12),
    ) {
        write_u32(t, ptr, (l as i32 - dx) as u32);
        write_u32(t, ptr + 4, (t_val as i32 - dy) as u32);
        write_u32(t, ptr + 8, (r as i32 + dx) as u32);
        write_u32(t, ptr + 12, (b as i32 + dy) as u32);
    }
    Ok(1)
}

fn draw_icon(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn draw_icon_ex(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn draw_edge(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn draw_frame_control(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn set_rect(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let ptr = a[0];
    write_u32(t, ptr, a[1] as u32);
    write_u32(t, ptr + 4, a[2] as u32);
    write_u32(t, ptr + 8, a[3] as u32);
    write_u32(t, ptr + 12, a[4] as u32);
    Ok(1)
}

fn pt_in_rect(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_class_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hwnd, buf, cch) = (a[0], a[1], a[2] as u32);
    let class =
        t.k.ui
            .window(hwnd)
            .map(|w| w.class.clone())
            .unwrap_or_default();
    let (n, _) = copy_str_truncate(t, buf, cch as u64, &class, wide);
    Ok(n)
}

fn set_window_long_ptr(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    set_window_long(t, a, wide)
}

fn get_window_long_ptr(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    get_window_long(t, a, wide)
}

fn get_desktop_window(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1) // fake desktop HWND
}

fn release_dc(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_window_dc(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn monitor_from_window(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1) // primary monitor handle
}

fn monitor_from_point(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn get_monitor_info(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // MONITORINFO: cbSize(4) rcMonitor(16) rcWork(16) dwFlags(4)
    let ptr = a[1];
    write_u32(t, ptr, 40); // cbSize
    write_u32(t, ptr + 4, 0); // left
    write_u32(t, ptr + 8, 0); // top
    write_u32(t, ptr + 12, 1920); // right
    write_u32(t, ptr + 16, 1080); // bottom
    write_u32(t, ptr + 20, 0); // work left
    write_u32(t, ptr + 24, 0); // work top
    write_u32(t, ptr + 28, 1920); // work right
    write_u32(t, ptr + 32, 1040); // work bottom (minus taskbar)
    write_u32(t, ptr + 36, 1); // MONITORINFOF_PRIMARY
    Ok(1)
}

fn multi_mon_enum_displays(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

// Shell / comdlg stubs

fn shell_execute(_t: &mut Thread<'_, '_>, _a: &[u64], _wide: bool) -> Result<u64, Halt> {
    Ok(42) // > 32 means success
}

fn shell_execute_ex(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn sh_get_folder_path(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let path_ptr = a[2];
    let s = r"C:\Users\User\AppData\Roaming";
    let (_, overflow) = copy_str_truncate(t, path_ptr, 260, s, wide);
    if overflow {
        fail(t, ERROR_INSUFFICIENT_BUFFER, 1)
    } else {
        Ok(0)
    }
}

fn sh_get_special_folder_path(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let path_ptr = a[1];
    let s = r"C:\Users\User\AppData\Roaming";
    let (_, overflow) = copy_str_truncate(t, path_ptr, 260, s, wide);
    Ok(!overflow as u64)
}

fn get_open_file_name(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0) // user cancelled
}

fn get_save_file_name(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn browse_for_folder(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn sh_browse_for_folder(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn sh_get_path_from_id_list(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn choose_color(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn choose_font(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

// OLE / COM stubs frequently imported by Inno Setup

fn co_initialize(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0) // S_OK
}

fn co_initialize_ex(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn co_uninitialize(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn co_create_instance(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0x8004_0154u64) // REGDB_E_CLASSNOTREG — caller typically handles this
}

fn co_get_class_object(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0x8004_0154u64)
}

fn ole_initialize(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn ole_uninitialize(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn sys_alloc_string(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Allocate a BSTR (len(4) + chars + null) on the process heap.
    let src = a[0];
    let Some(units) = crate::api::read_wide_units(t, src) else {
        return Ok(0);
    };
    let len_bytes = (units.len() * 2) as u32;
    let total = 4 + len_bytes as u64 + 2; // prefix + chars + null
    let addr = match t.process.memory.map_anywhere(
        total.max(16),
        anyos_memory::Protection::RW,
        "BSTR",
        None,
    ) {
        Ok(a) => a,
        Err(_) => return Ok(0),
    };
    write_u32(t, addr, len_bytes);
    for (i, &u) in units.iter().enumerate() {
        write_u16(t, addr + 4 + i as u64 * 2, u);
    }
    write_u16(t, addr + 4 + len_bytes as u64, 0);
    Ok(addr + 4) // BSTR points past the length prefix
}

fn sys_free_string(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn variant_init(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Zero the VARIANT (16 bytes on x86).
    let ptr = a[0];
    for i in 0..4u64 {
        write_u32(t, ptr + i * 4, 0);
    }
    Ok(0)
}

fn variant_clear(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    variant_init(t, a)
}

// ---------------------------------------------------------------------------
// comctl32

fn init_common_controls(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn init_common_controls_ex(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn image_list_create(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn image_list_destroy(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn image_list_add(t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(t.k.ui.next_gdi())
}

fn image_list_replace_icon(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0) // index 0
}

fn image_list_draw(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

fn property_sheet(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(0)
}

fn create_status_window(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let text = if a[1] != 0 {
        read_str(t, a[1], wide).unwrap_or_default()
    } else {
        String::new()
    };
    let hwnd = t.k.ui.create_window(
        "msctls_statusbar32".into(),
        text,
        a[0] as u32,
        0,
        0,
        0,
        640,
        22,
        a[2],
        a[3],
    );
    Ok(hwnd)
}

fn create_toolbar_ex(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let hwnd = t.k.ui.create_window(
        "ToolbarWindow32".into(),
        String::new(),
        0,
        0,
        0,
        0,
        640,
        26,
        a[0],
        0,
    );
    Ok(hwnd)
}

fn flat_sb_enum_procs(_t: &mut Thread<'_, '_>, _a: &[u64]) -> Result<u64, Halt> {
    Ok(1)
}

// ---------------------------------------------------------------------------
// API tables

const fn u32api(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("user32", name, args, f)
}

const fn g32api(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("gdi32", name, args, f)
}

const fn comctl(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("comctl32", name, args, f)
}

const fn shell(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("shell32", name, args, f)
}

const fn comdlg(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("comdlg32", name, args, f)
}

const fn oleaut(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("oleaut32", name, args, f)
}

const fn ole32a(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("ole32", name, args, f)
}

pub(crate) static APIS: &[Api] = &[
    // --- user32: window management ---
    u32api("RegisterClassA", 1, |t, a| register_class(t, a, false)),
    u32api("RegisterClassW", 1, |t, a| register_class(t, a, true)),
    u32api("RegisterClassExA", 1, |t, a| register_class(t, a, false)),
    u32api("RegisterClassExW", 1, |t, a| register_class(t, a, true)),
    u32api("UnregisterClassA", 2, |_, _| Ok(1)),
    u32api("UnregisterClassW", 2, |_, _| Ok(1)),
    u32api("CreateWindowA", 11, |t, a| {
        create_window_ex(
            t,
            &[
                0, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9],
            ],
            false,
        )
    }),
    u32api("CreateWindowW", 11, |t, a| {
        create_window_ex(
            t,
            &[
                0, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9],
            ],
            true,
        )
    }),
    u32api("CreateWindowExA", 12, |t, a| create_window_ex(t, a, false)),
    u32api("CreateWindowExW", 12, |t, a| create_window_ex(t, a, true)),
    u32api("DestroyWindow", 1, destroy_window),
    u32api("ShowWindow", 2, show_window),
    u32api("UpdateWindow", 1, update_window),
    u32api("RedrawWindow", 4, redraw_window),
    u32api("InvalidateRect", 3, invalidate_rect),
    u32api("InvalidateRgn", 3, |_, _| Ok(1)),
    u32api("ValidateRect", 2, |_, _| Ok(1)),
    u32api("GetWindowTextA", 3, |t, a| get_window_text(t, a, false)),
    u32api("GetWindowTextW", 3, |t, a| get_window_text(t, a, true)),
    u32api("GetWindowTextLengthA", 1, |t, a| {
        Ok(t.k
            .ui
            .window(a[0])
            .map(|w| w.title.len() as u64)
            .unwrap_or(0))
    }),
    u32api("GetWindowTextLengthW", 1, |t, a| {
        Ok(t.k
            .ui
            .window(a[0])
            .map(|w| w.title.len() as u64)
            .unwrap_or(0))
    }),
    u32api("SetWindowTextA", 2, |t, a| set_window_text(t, a, false)),
    u32api("SetWindowTextW", 2, |t, a| set_window_text(t, a, true)),
    u32api("SetWindowPos", 7, set_window_pos),
    u32api("MoveWindow", 6, |t, a| {
        if let Some(w) = t.k.ui.window_mut(a[0]) {
            w.x = a[1] as i32;
            w.y = a[2] as i32;
            w.w = a[3] as i32;
            w.h = a[4] as i32;
            w.visible = a[5] != 0 || w.visible;
        }
        Ok(1)
    }),
    u32api("GetWindowRect", 2, get_window_rect),
    u32api("GetClientRect", 2, get_client_rect),
    u32api("AdjustWindowRect", 3, adjust_window_rect),
    u32api("AdjustWindowRectEx", 4, adjust_window_rect),
    u32api("EnableWindow", 2, enable_window),
    u32api("IsWindowEnabled", 1, is_window_enabled),
    u32api("IsWindowVisible", 1, is_window_visible),
    u32api("IsWindow", 1, is_window),
    u32api("GetParent", 1, get_parent),
    u32api("GetWindow", 2, get_window),
    u32api("GetForegroundWindow", 0, get_foreground_window),
    u32api("SetForegroundWindow", 1, set_foreground_window),
    u32api("GetDesktopWindow", 0, get_desktop_window),
    u32api("FindWindowA", 2, |t, a| find_window(t, a, false)),
    u32api("FindWindowW", 2, |t, a| find_window(t, a, true)),
    u32api("FindWindowExA", 4, |t, a| find_window(t, &a[2..], false)),
    u32api("FindWindowExW", 4, |t, a| find_window(t, &a[2..], true)),
    u32api("GetWindowThreadProcessId", 2, get_window_thread_process_id),
    u32api("GetClassNameA", 3, |t, a| get_class_name(t, a, false)),
    u32api("GetClassNameW", 3, |t, a| get_class_name(t, a, true)),
    u32api("EnumWindows", 2, enum_windows),
    u32api("EnumChildWindows", 3, enum_child_windows),
    // --- user32: window properties ---
    u32api("GetWindowLongA", 2, |t, a| get_window_long(t, a, false)),
    u32api("GetWindowLongW", 2, |t, a| get_window_long(t, a, true)),
    u32api("SetWindowLongA", 3, |t, a| set_window_long(t, a, false)),
    u32api("SetWindowLongW", 3, |t, a| set_window_long(t, a, true)),
    u32api("GetWindowLongPtrA", 2, |t, a| {
        get_window_long_ptr(t, a, false)
    }),
    u32api("GetWindowLongPtrW", 2, |t, a| {
        get_window_long_ptr(t, a, true)
    }),
    u32api("SetWindowLongPtrA", 3, |t, a| {
        set_window_long_ptr(t, a, false)
    }),
    u32api("SetWindowLongPtrW", 3, |t, a| {
        set_window_long_ptr(t, a, true)
    }),
    u32api("GetPropA", 2, |_, _| Ok(0)),
    u32api("GetPropW", 2, |_, _| Ok(0)),
    u32api("SetPropA", 3, |_, _| Ok(1)),
    u32api("SetPropW", 3, |_, _| Ok(1)),
    u32api("RemovePropA", 2, |_, _| Ok(0)),
    u32api("RemovePropW", 2, |_, _| Ok(0)),
    // --- user32: message loop ---
    u32api("GetMessageA", 4, |t, a| peek_get_message(t, a, true, true)),
    u32api("GetMessageW", 4, |t, a| peek_get_message(t, a, true, true)),
    u32api("PeekMessageA", 5, |t, a| {
        peek_get_message(t, a, (a[4] & 1) != 0, false)
    }),
    u32api("PeekMessageW", 5, |t, a| {
        peek_get_message(t, a, (a[4] & 1) != 0, false)
    }),
    u32api("TranslateMessage", 1, translate_message),
    u32api("DispatchMessageA", 1, dispatch_message),
    u32api("DispatchMessageW", 1, dispatch_message),
    u32api("PostMessageA", 4, |t, a| post_message(t, a, false)),
    u32api("PostMessageW", 4, |t, a| post_message(t, a, true)),
    u32api("PostThreadMessageA", 4, |t, a| post_message(t, a, false)),
    u32api("PostThreadMessageW", 4, |t, a| post_message(t, a, true)),
    u32api("SendMessageA", 4, |t, a| send_message(t, a, false)),
    u32api("SendMessageW", 4, |t, a| send_message(t, a, true)),
    u32api("SendMessageTimeoutA", 6, |t, a| send_message(t, a, false)),
    u32api("SendMessageTimeoutW", 6, |t, a| send_message(t, a, true)),
    u32api("SendNotifyMessageA", 4, |t, a| post_message(t, a, false)),
    u32api("SendNotifyMessageW", 4, |t, a| post_message(t, a, true)),
    u32api("ReplyMessage", 1, |_, _| Ok(0)),
    u32api("PostQuitMessage", 1, post_quit_message),
    u32api("WaitMessage", 0, wait_message),
    u32api("GetMessagePos", 0, get_message_pos),
    u32api("GetMessageTime", 0, get_message_time),
    u32api("InSendMessage", 0, |_, _| Ok(0)),
    u32api("RegisterWindowMessageA", 1, |t, a| {
        register_window_message(t, a, false)
    }),
    u32api("RegisterWindowMessageW", 1, |t, a| {
        register_window_message(t, a, true)
    }),
    // --- user32: dialog ---
    u32api("MessageBoxA", 4, |t, a| message_box(t, a, false)),
    u32api("MessageBoxW", 4, |t, a| message_box(t, a, true)),
    u32api("MessageBoxExA", 5, |t, a| message_box(t, a, false)),
    u32api("MessageBoxExW", 5, |t, a| message_box(t, a, true)),
    u32api("MessageBoxIndirectA", 1, |t, a| {
        message_box(t, &[a[0], 0, 0, 0], false)
    }),
    u32api("MessageBoxIndirectW", 1, |t, a| {
        message_box(t, &[a[0], 0, 0, 0], true)
    }),
    u32api("DialogBoxParamA", 5, |t, a| dialog_box_param(t, a, false)),
    u32api("DialogBoxParamW", 5, |t, a| dialog_box_param(t, a, true)),
    u32api("DialogBoxIndirectParamA", 5, |t, a| {
        dialog_box_param(t, a, false)
    }),
    u32api("DialogBoxIndirectParamW", 5, |t, a| {
        dialog_box_param(t, a, true)
    }),
    u32api("CreateDialogParamA", 5, |t, a| {
        dialog_box_param(t, a, false)
    }),
    u32api("CreateDialogParamW", 5, |t, a| dialog_box_param(t, a, true)),
    u32api("CreateDialogIndirectParamA", 5, |t, a| {
        dialog_box_param(t, a, false)
    }),
    u32api("CreateDialogIndirectParamW", 5, |t, a| {
        dialog_box_param(t, a, true)
    }),
    u32api("EndDialog", 2, end_dialog),
    u32api("GetDlgItem", 2, get_dlg_item),
    u32api("SetDlgItemTextA", 3, |t, a| set_dlg_item_text(t, a, false)),
    u32api("SetDlgItemTextW", 3, |t, a| set_dlg_item_text(t, a, true)),
    u32api("GetDlgItemTextA", 4, |t, a| get_dlg_item_text(t, a, false)),
    u32api("GetDlgItemTextW", 4, |t, a| get_dlg_item_text(t, a, true)),
    u32api("GetDlgItemInt", 3, |_, _| Ok(0)),
    u32api("SetDlgItemInt", 4, |_, _| Ok(1)),
    u32api("SendDlgItemMessageA", 5, |t, a| {
        send_dlg_item_message(t, a, false)
    }),
    u32api("SendDlgItemMessageW", 5, |t, a| {
        send_dlg_item_message(t, a, true)
    }),
    u32api("CheckDlgButton", 3, check_dlg_button),
    u32api("IsDlgButtonChecked", 2, is_dlg_button_checked),
    u32api("CheckRadioButton", 4, |_, _| Ok(1)),
    u32api("IsDialogMessageA", 2, |_, _| Ok(0)),
    u32api("IsDialogMessageW", 2, |_, _| Ok(0)),
    u32api("MapDialogRect", 2, |_, _| Ok(1)),
    u32api("DefDlgProcA", 4, |t, a| def_window_proc(t, a, false)),
    u32api("DefDlgProcW", 4, |t, a| def_window_proc(t, a, true)),
    // --- user32: DC / paint ---
    u32api("GetDC", 1, get_dc),
    u32api("GetWindowDC", 1, get_window_dc),
    u32api("ReleaseDC", 2, release_dc),
    u32api("BeginPaint", 2, begin_paint),
    u32api("EndPaint", 2, end_paint),
    u32api("FillRect", 3, fill_rect),
    // --- user32: focus / input ---
    u32api("SetFocus", 1, set_focus),
    u32api("GetFocus", 0, get_focus),
    u32api("SetCapture", 1, set_capture),
    u32api("ReleaseCapture", 0, release_capture),
    u32api("GetCursorPos", 1, get_cursor_pos),
    u32api("SetCursorPos", 2, |_, _| Ok(1)),
    u32api("GetAsyncKeyState", 1, get_async_key_state),
    u32api("GetKeyState", 1, get_key_state),
    u32api("GetKeyboardLayout", 1, |_, _| Ok(0x0409_0409u64)), // US English
    u32api("GetKeyboardState", 1, |_, _| Ok(1)),
    u32api("SetKeyboardState", 1, |_, _| Ok(1)),
    u32api("MapVirtualKeyA", 2, |_, _| Ok(0)),
    u32api("MapVirtualKeyW", 2, |_, _| Ok(0)),
    u32api("VkKeyScanA", 1, |_, _| Ok(0xFFFF)),
    u32api("VkKeyScanW", 1, |_, _| Ok(0xFFFF)),
    // --- user32: system metrics ---
    u32api("GetSystemMetrics", 1, get_system_metrics),
    u32api("SystemParametersInfoA", 4, system_parameters_info),
    u32api("SystemParametersInfoW", 4, system_parameters_info),
    u32api("GetSysColor", 1, get_sys_color),
    u32api("GetSysColorBrush", 1, get_sys_color_brush),
    // --- user32: resources / cursors ---
    u32api("LoadCursorA", 2, load_cursor),
    u32api("LoadCursorW", 2, load_cursor),
    u32api("LoadIconA", 2, load_icon),
    u32api("LoadIconW", 2, load_icon),
    u32api("LoadBitmapA", 2, load_bitmap),
    u32api("LoadBitmapW", 2, load_bitmap),
    u32api("LoadImageA", 5, load_image),
    u32api("LoadImageW", 5, load_image),
    u32api("SetCursor", 1, set_cursor),
    u32api("DestroyCursor", 1, |_, _| Ok(1)),
    u32api("DestroyIcon", 1, |_, _| Ok(1)),
    u32api("CopyIcon", 1, |t, a| Ok(t.k.ui.next_gdi())),
    u32api("LoadAcceleratorsA", 2, load_accelerators),
    u32api("LoadAcceleratorsW", 2, load_accelerators),
    u32api("TranslateAcceleratorA", 3, translate_accelerator),
    u32api("TranslateAcceleratorW", 3, translate_accelerator),
    // --- user32: timers ---
    u32api("SetTimer", 4, set_timer),
    u32api("KillTimer", 2, kill_timer),
    // --- user32: menu stubs ---
    u32api("GetMenu", 1, |t, a| Ok(t.k.ui.next_gdi())),
    u32api("SetMenu", 2, |_, _| Ok(1)),
    u32api("CreateMenu", 0, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("CreatePopupMenu", 0, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("DestroyMenu", 1, |_, _| Ok(1)),
    u32api("AppendMenuA", 4, |_, _| Ok(1)),
    u32api("AppendMenuW", 4, |_, _| Ok(1)),
    u32api("InsertMenuItemA", 4, |_, _| Ok(1)),
    u32api("InsertMenuItemW", 4, |_, _| Ok(1)),
    u32api("SetMenuItemInfoA", 4, |_, _| Ok(1)),
    u32api("SetMenuItemInfoW", 4, |_, _| Ok(1)),
    u32api("GetMenuItemInfoA", 4, |_, _| Ok(0)),
    u32api("GetMenuItemInfoW", 4, |_, _| Ok(0)),
    u32api("CheckMenuItem", 3, |_, _| Ok(0)),
    u32api("EnableMenuItem", 3, |_, _| Ok(0)),
    u32api("DrawMenuBar", 1, |_, _| Ok(1)),
    u32api("TrackPopupMenu", 7, |_, _| Ok(0)),
    u32api("TrackPopupMenuEx", 6, |_, _| Ok(0)),
    u32api("GetMenuItemCount", 1, |_, _| Ok(0)),
    u32api("GetSubMenu", 2, |_, _| Ok(0)),
    // --- user32: misc ---
    u32api("DefWindowProcA", 4, |t, a| def_window_proc(t, a, false)),
    u32api("DefWindowProcW", 4, |t, a| def_window_proc(t, a, true)),
    u32api("CallWindowProcA", 5, |t, a| call_window_proc(t, a, false)),
    u32api("CallWindowProcW", 5, |t, a| call_window_proc(t, a, true)),
    u32api("ScreenToClient", 2, screen_to_client),
    u32api("ClientToScreen", 2, client_to_screen),
    u32api("MapWindowPoints", 4, map_window_points),
    u32api("CopyRect", 2, copy_rect),
    u32api("InflateRect", 3, inflate_rect),
    u32api("SetRect", 5, set_rect),
    u32api("PtInRect", 3, pt_in_rect),
    u32api("DrawIcon", 4, draw_icon),
    u32api("DrawIconEx", 9, draw_icon_ex),
    u32api("DrawEdge", 4, draw_edge),
    u32api("DrawFrameControl", 4, draw_frame_control),
    u32api("MonitorFromWindow", 2, monitor_from_window),
    u32api("MonitorFromPoint", 2, monitor_from_point),
    u32api("GetMonitorInfoA", 2, get_monitor_info),
    u32api("GetMonitorInfoW", 2, get_monitor_info),
    u32api("EnumDisplayMonitors", 4, multi_mon_enum_displays),
    u32api("SetLayeredWindowAttributes", 4, |_, _| Ok(1)),
    u32api("GetLayeredWindowAttributes", 4, |_, _| Ok(0)),
    u32api("AnimateWindow", 3, |_, _| Ok(1)),
    u32api("FlashWindow", 2, |_, _| Ok(0)),
    u32api("FlashWindowEx", 1, |_, _| Ok(0)),
    u32api("BringWindowToTop", 1, |_, _| Ok(1)),
    u32api("SetActiveWindow", 1, |_, a| Ok(a[0])),
    u32api("GetActiveWindow", 0, |t, _| {
        Ok(t.k.ui.windows.first().map(|w| w.hwnd).unwrap_or(0))
    }),
    u32api("GetTopWindow", 1, |t, _| {
        Ok(t.k.ui.windows.first().map(|w| w.hwnd).unwrap_or(0))
    }),
    u32api("SetWindowRgn", 3, |_, _| Ok(1)),
    u32api("SetParent", 2, |t, a| {
        let old = t.k.ui.window(a[0]).map(|w| w.parent).unwrap_or(0);
        if let Some(w) = t.k.ui.window_mut(a[0]) {
            w.parent = a[1];
        }
        Ok(old)
    }),
    u32api("GetNextDlgTabItem", 3, |_, a| Ok(a[1])),
    u32api("GetNextDlgGroupItem", 3, |_, a| Ok(a[1])),
    u32api("WinHelpA", 4, |_, _| Ok(1)),
    u32api("WinHelpW", 4, |_, _| Ok(1)),
    u32api("OpenClipboard", 1, |_, _| Ok(1)),
    u32api("CloseClipboard", 0, |_, _| Ok(1)),
    u32api("EmptyClipboard", 0, |_, _| Ok(1)),
    u32api("GetClipboardData", 1, |_, _| Ok(0)),
    u32api("SetClipboardData", 2, |_, a| Ok(a[1])),
    u32api("IsClipboardFormatAvailable", 1, |_, _| Ok(0)),
    u32api("CountClipboardFormats", 0, |_, _| Ok(0)),
    u32api("RegisterClipboardFormatA", 1, |t, _| {
        Ok(0xC000 + t.k.ui.classes.len() as u64)
    }),
    u32api("RegisterClipboardFormatW", 1, |t, _| {
        Ok(0xC000 + t.k.ui.classes.len() as u64)
    }),
    u32api("GetCaretPos", 1, |t, a| {
        write_u32(t, a[0], 0);
        write_u32(t, a[0] + 4, 0);
        Ok(1)
    }),
    u32api("SetCaretPos", 2, |_, _| Ok(1)),
    u32api("CreateCaret", 4, |_, _| Ok(1)),
    u32api("DestroyCaret", 0, |_, _| Ok(1)),
    u32api("ShowCaret", 1, |_, _| Ok(1)),
    u32api("HideCaret", 1, |_, _| Ok(1)),
    u32api("GetScrollInfo", 3, |_, _| Ok(0)),
    u32api("SetScrollInfo", 4, |_, _| Ok(0)),
    u32api("SetScrollPos", 4, |_, _| Ok(0)),
    u32api("GetScrollPos", 2, |_, _| Ok(0)),
    u32api("ShowScrollBar", 3, |_, _| Ok(1)),
    u32api("EnableScrollBar", 3, |_, _| Ok(1)),
    u32api("SetScrollRange", 5, |_, _| Ok(1)),
    u32api("GetScrollRange", 4, |_, _| Ok(1)),
    u32api("CreateRoundRectRgn", 7, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("CreateRectRgn", 4, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("CreateEllipticRgn", 4, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("SetWindowsHookExA", 4, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("SetWindowsHookExW", 4, |t, _| Ok(t.k.ui.next_gdi())),
    u32api("UnhookWindowsHookEx", 1, |_, _| Ok(1)),
    u32api("CallNextHookEx", 4, |_, _| Ok(0)),
    u32api("GetSubMenu", 2, |_, _| Ok(0)),
    u32api("DrawText", 5, |t, a| draw_text(t, a, false)),
    u32api("DrawTextW", 5, |t, a| draw_text(t, a, true)),
    u32api("DrawTextExA", 6, |t, a| draw_text(t, a, false)),
    u32api("DrawTextExW", 6, |t, a| draw_text(t, a, true)),
    u32api("TabbedTextOutA", 8, |t, a| text_out(t, a, false)),
    u32api("TabbedTextOutW", 8, |t, a| text_out(t, a, true)),
    u32api("FrameRect", 3, |_, _| Ok(1)),
    // --- gdi32 ---
    g32api("CreateFontA", 14, create_font),
    g32api("CreateFontW", 14, create_font),
    g32api("CreateFontIndirectA", 1, create_font),
    g32api("CreateFontIndirectW", 1, create_font),
    g32api("CreateFontIndirectExA", 1, create_font),
    g32api("CreateFontIndirectExW", 1, create_font),
    g32api("DeleteObject", 1, delete_object),
    g32api("GetStockObject", 1, get_stock_object),
    g32api("SelectObject", 2, select_object),
    g32api("CreateSolidBrush", 1, create_solid_brush),
    g32api("CreateHatchBrush", 2, create_solid_brush),
    g32api("CreatePatternBrush", 1, create_solid_brush),
    g32api("CreateBrushIndirect", 1, create_solid_brush),
    g32api("CreatePen", 3, create_pen),
    g32api("CreatePenIndirect", 1, create_pen),
    g32api("CreateCompatibleDC", 1, create_compatible_dc),
    g32api("CreateCompatibleBitmap", 3, create_compatible_bitmap),
    g32api("CreateBitmap", 5, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CreateDIBSection", 6, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CreateDIBitmap", 6, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("DeleteDC", 1, delete_dc),
    g32api("BitBlt", 9, bit_blt),
    g32api("StretchBlt", 11, stretch_blt),
    g32api("SetStretchBltMode", 2, set_stretch_blt_mode),
    g32api("PatBlt", 6, |_, _| Ok(1)),
    g32api("TextOutA", 5, |t, a| text_out(t, a, false)),
    g32api("TextOutW", 5, |t, a| text_out(t, a, true)),
    g32api("ExtTextOutA", 8, |t, a| ext_text_out(t, a, false)),
    g32api("ExtTextOutW", 8, |t, a| ext_text_out(t, a, true)),
    g32api("DrawTextA", 5, |t, a| draw_text(t, a, false)),
    g32api("DrawTextW", 5, |t, a| draw_text(t, a, true)),
    g32api("Rectangle", 5, rectangle),
    g32api("Ellipse", 5, ellipse),
    g32api("RoundRect", 7, |_, _| Ok(1)),
    g32api("Polygon", 3, |_, _| Ok(1)),
    g32api("Polyline", 3, |_, _| Ok(1)),
    g32api("LineTo", 3, line_to),
    g32api("MoveToEx", 4, move_to_ex),
    g32api("Arc", 9, |_, _| Ok(1)),
    g32api("Chord", 9, |_, _| Ok(1)),
    g32api("Pie", 9, |_, _| Ok(1)),
    g32api("SetBkMode", 2, set_bk_mode),
    g32api("GetBkMode", 1, |_, _| Ok(2)), // OPAQUE
    g32api("SetBkColor", 2, set_bk_color),
    g32api("GetBkColor", 1, |_, _| Ok(0xFFFFFF)),
    g32api("SetTextColor", 2, set_text_color),
    g32api("GetTextColor", 1, |_, _| Ok(0)),
    g32api("SetTextAlign", 2, |_, a| Ok(a[1])),
    g32api("GetTextAlign", 1, |_, _| Ok(0)),
    g32api("GetTextMetricsA", 2, get_text_metrics),
    g32api("GetTextMetricsW", 2, get_text_metrics),
    g32api("GetTextExtentPoint32A", 4, |t, a| {
        get_text_extent_point(t, a, false)
    }),
    g32api("GetTextExtentPoint32W", 4, |t, a| {
        get_text_extent_point(t, a, true)
    }),
    g32api("GetTextExtentExPointA", 7, |t, a| {
        get_text_extent_point(t, a, false)
    }),
    g32api("GetTextExtentExPointW", 7, |t, a| {
        get_text_extent_point(t, a, true)
    }),
    g32api("GetCharWidthA", 4, |_, _| Ok(1)),
    g32api("GetCharWidthW", 4, |_, _| Ok(1)),
    g32api("GetCharWidth32A", 4, |_, _| Ok(1)),
    g32api("GetCharWidth32W", 4, |_, _| Ok(1)),
    g32api("GetDeviceCaps", 2, get_device_caps),
    g32api("GetDCOrgEx", 2, |t, a| {
        write_u32(t, a[1], 0);
        write_u32(t, a[1] + 4, 0);
        Ok(1)
    }),
    g32api("SaveDC", 1, |_, _| Ok(1)),
    g32api("RestoreDC", 2, |_, _| Ok(1)),
    g32api("SetROP2", 2, |_, _| Ok(0)),
    g32api("GetROP2", 1, |_, _| Ok(2)),
    g32api("SetMapMode", 2, |_, _| Ok(1)),
    g32api("GetMapMode", 1, |_, _| Ok(1)),
    g32api("SetViewportOrgEx", 3, |_, _| Ok(1)),
    g32api("SetWindowOrgEx", 3, |_, _| Ok(1)),
    g32api("SetViewportExtEx", 3, |_, _| Ok(1)),
    g32api("SetWindowExtEx", 3, |_, _| Ok(1)),
    g32api("ScaleViewportExtEx", 5, |_, _| Ok(1)),
    g32api("ScaleWindowExtEx", 5, |_, _| Ok(1)),
    g32api("GetViewportOrgEx", 2, |t, a| {
        write_u32(t, a[1], 0);
        write_u32(t, a[1] + 4, 0);
        Ok(1)
    }),
    g32api("GetWindowOrgEx", 2, |t, a| {
        write_u32(t, a[1], 0);
        write_u32(t, a[1] + 4, 0);
        Ok(1)
    }),
    g32api("IntersectClipRect", 5, |_, _| Ok(2)), // SIMPLEREGION
    g32api("ExcludeClipRect", 5, |_, _| Ok(2)),
    g32api("SelectClipRgn", 2, |_, _| Ok(2)),
    g32api("GetClipRgn", 2, |_, _| Ok(0)),
    g32api("GetClipBox", 2, |t, a| {
        write_u32(t, a[1], 0);
        write_u32(t, a[1] + 4, 0);
        write_u32(t, a[1] + 8, 1920);
        write_u32(t, a[1] + 12, 1080);
        Ok(1)
    }),
    g32api("PtVisible", 3, |_, _| Ok(1)),
    g32api("RectVisible", 2, |_, _| Ok(1)),
    g32api("GdiFlush", 0, |_, _| Ok(1)),
    g32api("GdiGetBatchLimit", 0, |_, _| Ok(1)),
    g32api("GdiSetBatchLimit", 1, |_, _| Ok(1)),
    g32api("GetObjectA", 3, |_, _| Ok(0)),
    g32api("GetObjectW", 3, |_, _| Ok(0)),
    g32api("GetObjectType", 1, |_, _| Ok(0)),
    g32api("CreateRectRgn", 4, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CombineRgn", 4, |_, _| Ok(2)),
    g32api("FillRgn", 3, |_, _| Ok(1)),
    g32api("FrameRgn", 5, |_, _| Ok(1)),
    g32api("EqualRgn", 2, |_, _| Ok(0)),
    g32api("GetRegionData", 3, |_, _| Ok(0)),
    g32api("OffsetRgn", 3, |_, _| Ok(2)),
    g32api("CreateICW", 4, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CreateICA", 4, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CreateDCW", 4, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CreateDCA", 4, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("RealizePalette", 1, |_, _| Ok(0)),
    g32api("SelectPalette", 3, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("CreatePalette", 1, |t, _| Ok(t.k.ui.next_gdi())),
    g32api("GetNearestColor", 2, |_, a| Ok(a[1])),
    g32api("GetSystemPaletteEntries", 4, |_, _| Ok(0)),
    g32api("GetDIBits", 7, |_, _| Ok(0)),
    g32api("SetDIBits", 7, |_, _| Ok(0)),
    g32api("SetDIBitsToDevice", 13, |_, _| Ok(0)),
    g32api("StretchDIBits", 13, |_, _| Ok(0)),
    g32api("GetBitmapBits", 3, |_, _| Ok(0)),
    g32api("SetBitmapBits", 3, |_, _| Ok(0)),
    g32api("GetBitmapDimensionEx", 2, |_, _| Ok(0)),
    g32api("SetBitmapDimensionEx", 4, |_, _| Ok(1)),
    g32api("LPtoDP", 3, |_, _| Ok(1)),
    g32api("DPtoLP", 3, |_, _| Ok(1)),
    // --- comctl32 ---
    comctl("InitCommonControls", 0, init_common_controls),
    comctl("InitCommonControlsEx", 1, init_common_controls_ex),
    comctl("ImageList_Create", 5, image_list_create),
    comctl("ImageList_Destroy", 1, image_list_destroy),
    comctl("ImageList_Add", 3, image_list_add),
    comctl("ImageList_ReplaceIcon", 3, image_list_replace_icon),
    comctl("ImageList_Draw", 6, image_list_draw),
    comctl("ImageList_DrawEx", 8, image_list_draw),
    comctl("ImageList_GetIcon", 3, |t, _| Ok(t.k.ui.next_gdi())),
    comctl("ImageList_LoadImageA", 7, |t, _| Ok(t.k.ui.next_gdi())),
    comctl("ImageList_LoadImageW", 7, |t, _| Ok(t.k.ui.next_gdi())),
    comctl("PropertySheetA", 1, property_sheet),
    comctl("PropertySheetW", 1, property_sheet),
    comctl("CreateStatusWindowA", 4, |t, a| {
        create_status_window(t, a, false)
    }),
    comctl("CreateStatusWindowW", 4, |t, a| {
        create_status_window(t, a, true)
    }),
    comctl("CreateToolbarEx", 13, create_toolbar_ex),
    comctl("FlatSB_EnableScrollBar", 3, |_, _| Ok(1)),
    comctl("FlatSB_GetScrollInfo", 3, |_, _| Ok(0)),
    comctl("FlatSB_GetScrollPos", 2, |_, _| Ok(0)),
    comctl("FlatSB_SetScrollInfo", 4, |_, _| Ok(0)),
    comctl("FlatSB_SetScrollPos", 4, |_, _| Ok(0)),
    comctl("FlatSB_ShowScrollBar", 3, |_, _| Ok(0)),
    comctl("FlatSB_GetScrollRange", 4, |_, _| Ok(0)),
    comctl("FlatSB_SetScrollRange", 5, |_, _| Ok(0)),
    comctl("UninitializeFlatSB", 1, flat_sb_enum_procs),
    comctl("InitializeFlatSB", 1, flat_sb_enum_procs),
    comctl("_TrackMouseEvent", 1, |_, _| Ok(1)),
    comctl("TrackMouseEvent", 1, |_, _| Ok(1)),
    comctl("DrawStatusTextA", 4, |_, _| Ok(0)),
    comctl("DrawStatusTextW", 4, |_, _| Ok(0)),
    comctl("MenuHelp", 7, |_, _| Ok(0)),
    comctl("GetEffectiveClientRect", 3, |_, _| Ok(0)),
    comctl("ShowHideMenuCtl", 3, |_, _| Ok(0)),
    // --- shell32 ---
    shell("ShellExecuteA", 6, |t, a| shell_execute(t, a, false)),
    shell("ShellExecuteW", 6, |t, a| shell_execute(t, a, true)),
    shell("ShellExecuteExA", 1, shell_execute_ex),
    shell("ShellExecuteExW", 1, shell_execute_ex),
    shell("SHGetFolderPathA", 5, |t, a| {
        sh_get_folder_path(t, a, false)
    }),
    shell("SHGetFolderPathW", 5, |t, a| sh_get_folder_path(t, a, true)),
    shell("SHGetSpecialFolderPathA", 4, |t, a| {
        sh_get_special_folder_path(t, a, false)
    }),
    shell("SHGetSpecialFolderPathW", 4, |t, a| {
        sh_get_special_folder_path(t, a, true)
    }),
    shell("SHGetSpecialFolderLocation", 3, |_, _| Ok(1)),
    shell("SHGetPathFromIDListA", 2, sh_get_path_from_id_list),
    shell("SHGetPathFromIDListW", 2, sh_get_path_from_id_list),
    shell("SHBrowseForFolderA", 1, sh_browse_for_folder),
    shell("SHBrowseForFolderW", 1, sh_browse_for_folder),
    shell("SHCreateDirectoryExA", 3, |_, _| Ok(0)),
    shell("SHCreateDirectoryExW", 3, |_, _| Ok(0)),
    shell("SHFileOperationA", 1, |_, _| Ok(0)),
    shell("SHFileOperationW", 1, |_, _| Ok(0)),
    shell("SHGetFileInfoA", 5, |_, _| Ok(1)),
    shell("SHGetFileInfoW", 5, |_, _| Ok(1)),
    shell("SHGetMalloc", 1, |_, _| Ok(0)),
    shell("SHChangeNotify", 4, |_, _| Ok(0)),
    shell("ExtractIconA", 3, load_icon),
    shell("ExtractIconW", 3, load_icon),
    shell("ExtractIconExA", 5, |_, _| Ok(0)),
    shell("ExtractIconExW", 5, |_, _| Ok(0)),
    shell("FindExecutableA", 3, |_, _| Ok(0)),
    shell("FindExecutableW", 3, |_, _| Ok(0)),
    shell("DragAcceptFiles", 2, |_, _| Ok(0)),
    shell("DragQueryFileA", 4, |_, _| Ok(0)),
    shell("DragQueryFileW", 4, |_, _| Ok(0)),
    shell("DragFinish", 1, |_, _| Ok(0)),
    shell("Shell_NotifyIconA", 2, |_, _| Ok(1)),
    shell("Shell_NotifyIconW", 2, |_, _| Ok(1)),
    // --- comdlg32 ---
    comdlg("GetOpenFileNameA", 1, get_open_file_name),
    comdlg("GetOpenFileNameW", 1, get_open_file_name),
    comdlg("GetSaveFileNameA", 1, get_save_file_name),
    comdlg("GetSaveFileNameW", 1, get_save_file_name),
    comdlg("ChooseColorA", 1, choose_color),
    comdlg("ChooseColorW", 1, choose_color),
    comdlg("ChooseFontA", 1, choose_font),
    comdlg("ChooseFontW", 1, choose_font),
    comdlg("CommDlgExtendedError", 0, |_, _| Ok(0)),
    // --- ole32 ---
    ole32a("CoInitialize", 1, co_initialize),
    ole32a("CoInitializeEx", 2, co_initialize_ex),
    ole32a("CoUninitialize", 0, co_uninitialize),
    ole32a("CoCreateInstance", 5, co_create_instance),
    ole32a("CoGetClassObject", 5, co_get_class_object),
    ole32a("CoTaskMemAlloc", 1, |t, a| {
        match t.process.memory.map_anywhere(
            a[0].max(1),
            anyos_memory::Protection::RW,
            "CoTaskMem",
            None,
        ) {
            Ok(addr) => Ok(addr),
            Err(_) => Ok(0),
        }
    }),
    ole32a("CoTaskMemFree", 1, |_, _| Ok(0)),
    ole32a("CoTaskMemRealloc", 2, |_, _| Ok(0)),
    ole32a("StringFromGUID2", 3, |_, _| Ok(0)),
    ole32a("CLSIDFromString", 2, |_, _| Ok(0x8000_4005u64)), // E_INVALIDARG-ish
    ole32a("CLSIDFromProgID", 2, |_, _| Ok(0x8004_0154u64)),
    ole32a("OleInitialize", 1, ole_initialize),
    ole32a("OleUninitialize", 0, ole_uninitialize),
    // --- oleaut32 ---
    oleaut("SysAllocString", 1, sys_alloc_string),
    oleaut("SysAllocStringLen", 2, sys_alloc_string),
    oleaut("SysFreeString", 1, sys_free_string),
    oleaut("SysStringLen", 1, |t, a| {
        // BSTR: length prefix 4 bytes before the pointer
        let addr = a[0];
        if addr < 4 {
            return Ok(0);
        }
        Ok(read_u32(t, addr - 4).map(|n| (n / 2) as u64).unwrap_or(0))
    }),
    oleaut("SysStringByteLen", 1, |t, a| {
        let addr = a[0];
        if addr < 4 {
            return Ok(0);
        }
        Ok(read_u32(t, addr - 4).map(u64::from).unwrap_or(0))
    }),
    oleaut("VariantInit", 1, variant_init),
    oleaut("VariantClear", 1, variant_clear),
    oleaut("VariantCopy", 2, |t, a| variant_init(t, &a[..1])),
    oleaut("VariantChangeType", 4, |_, _| Ok(0x8002_0008u64)), // DISP_E_TYPEMISMATCH
    oleaut("SafeArrayCreate", 3, |_, _| Ok(0)),
    oleaut("SafeArrayDestroy", 1, |_, _| Ok(0)),
    oleaut("VarBstrFromI4", 4, |_, _| Ok(0)),
];
