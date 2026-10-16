# AnyOS Universal UI Layer Strategy

## Problem Statement

Windows programs expect to display UIs (windows, dialogs, graphics). AnyOS currently has stub implementations that fake the message loop but don't actually render anything. We need:

1. **Universal abstraction** for UI that works cross-platform
2. **Native rendering** on each platform (Metal on macOS, OpenGL/Vulkan on Linux/Windows)
3. **Win32 API translation** that maps `CreateWindowEx`, `CreateDC`, GDI calls, etc. to native rendering
4. **Execution capability** - Actually run Windows programs with visible UIs on macOS

## Architecture

```
Windows Program
    ↓
Win32 API Calls (CreateWindowEx, DrawText, etc.)
    ↓
┌─────────────────────────────────────────────────┐
│        AnyOS UI Layer (Rust)                    │
│  • Message dispatch                             │
│  • Handle-to-native-resource mapping            │
│  • Win32 → Platform semantics translation       │
└─────────────────────────────────────────────────┘
    ↓
┌────────────────┬─────────────────┬──────────────┐
│  macOS Native  │  Linux Native   │  Windows     │
│  (Metal/Cocoa) │  (Vulkan/GTK)   │  (Direct3D)  │
└────────────────┴─────────────────┴──────────────┘
```

## Implementation Phases

### Phase 1: Basic Window Rendering (2 weeks)
**Goal**: Get a Windows program window to appear on macOS with text rendering

**Tasks:**
1. Create `anyos-ui` crate with platform-agnostic UI trait
2. Implement `UIBackend` trait for macOS using Metal
3. Map `CreateWindowEx` → Native window creation
4. Map `CreateDC` → Drawing context
5. Implement text rendering (SelectObject, TextOutA, etc.)
6. Test with simple console-to-window program

**Deliverables:**
- A Windows program that creates a window renders text on macOS

### Phase 2: GDI Graphics Support (2 weeks)
**Goal**: Support basic drawing operations

**Tasks:**
1. Map GDI operations (LineTo, Rectangle, Ellipse, etc.)
2. Implement brush/pen selection
3. Add color support
4. Create test programs with simple graphics

**Deliverables:**
- Windows programs can draw shapes and lines

### Phase 3: Dialog Support (1 week)
**Goal**: Dialog boxes and controls

**Tasks:**
1. Map `DialogBoxIndirect` to native dialogs
2. Implement button, edit control, static text
3. Add message handling for dialog controls

**Deliverables:**
- Programs with dialog boxes run

### Phase 4: Graphics Pipeline (3 weeks)
**Goal**: DirectX/OpenGL support

**Tasks:**
1. Implement basic Direct3D 9 subset
2. Add shader support
3. Texture management

**Deliverables:**
- Simple 3D programs run

### Phase 5: macOS Execution Optimization (1 week)
**Goal**: Performance and edge cases

**Tasks:**
1. Native code execution (not interpreter-only)
2. Metal shader compilation
3. Performance profiling

**Deliverables:**
- Games and complex programs run at reasonable speed

## Win32 API Expansion Required

Current coverage: ~60% of `kernel32`

**Critical missing for UI:**
- [ ] User32 window creation/messaging (core)
- [ ] GDI object creation and manipulation (core)
- [ ] Dialog box support
- [ ] Menu APIs
- [ ] Event handling (mouse, keyboard)
- [ ] Clipboard
- [ ] DPI awareness
- [ ] COMctls (common controls)
- [ ] D3D 9-12 subset

**Estimated effort:**
- User32 + GDI basics: 4-6 weeks
- Complete GDI: 2-3 weeks
- Dialog/Controls: 2 weeks
- D3D: 4-6 weeks

## File Structure

```
crates/anyos-ui/
├── src/
│   ├── lib.rs              # Main trait definitions
│   ├── platform.rs         # Platform abstraction
│   ├── window.rs           # Window management
│   ├── graphics.rs         # Graphics context
│   └── platform/
│       ├── macos.rs        # Metal + Cocoa
│       ├── linux.rs        # Vulkan + GTK
│       └── windows.rs      # Direct3D
└── Cargo.toml

crates/anyos-win32/src/
├── ui.rs                   # Updated with real implementation
├── gdi.rs                  # NEW: GDI APIs
├── dialogs.rs              # NEW: Dialog support
└── graphics.rs             # NEW: Graphics APIs
```

## Implementation Strategy

### Step 1: Define Platform-Agnostic Traits

```rust
pub trait UIBackend: Send + Sync {
    fn create_window(&mut self, params: WindowParams) -> WindowHandle;
    fn destroy_window(&mut self, handle: WindowHandle);
    fn create_dc(&mut self, hwnd: WindowHandle) -> DCHandle;
    fn draw_text(&mut self, dc: DCHandle, text: &str, rect: Rect) -> Result;
    fn set_pixel(&mut self, dc: DCHandle, x: i32, y: i32, color: u32);
    fn blit(&mut self, dest_dc: DCHandle, src_dc: DCHandle, rect: Rect);
    fn message_loop_step(&mut self, timeout: Duration) -> Option<Message>;
}
```

### Step 2: macOS Implementation

Use existing `anyos-gui` as base:
- Reuse egui/glow infrastructure where possible
- Create Cocoa native windows
- Implement Metal rendering backend
- Event dispatching through Cocoa event loop

### Step 3: Wire into Win32 APIs

Update `crates/anyos-win32/src/ui.rs`:
- Replace stubs with real implementations
- Connect to UIBackend
- Maintain handle tables (HWND → Window, HDC → DrawingContext)

### Step 4: Incremental Testing

Start with simplest possible program:
```c
// test_window.exe
int main() {
    HWND hwnd = CreateWindowEx(0, "STATIC", "Hello, World!", 
                               WS_VISIBLE | WS_OVERLAPPEDWINDOW,
                               100, 100, 400, 300,
                               NULL, NULL, NULL, NULL);
    MSG msg;
    while (GetMessage(&msg, NULL, 0, 0)) {
        DispatchMessage(&msg);
    }
    return 0;
}
```

Then iterate with Notepad, Paint, etc.

## Dependencies

**New Crates Needed:**
- `wgpu` or `raw-metal-rs` - macOS Metal bindings
- `cocoa` - Cocoa framework bindings
- `objc` - Objective-C runtime
- (Keep `egui` for GUI testing/reference)

**Keep Existing:**
- `libc` - System calls
- `goblin` - PE parsing
- All AnyOS crates

## Success Metrics

| Milestone | Success | Timeline |
|-----------|---------|----------|
| Phase 1 | Simple window with text on macOS | Week 2 |
| Phase 2 | Basic drawing (shapes, colors) | Week 4 |
| Phase 3 | Dialog boxes work | Week 5 |
| Phase 4 | 3D graphics work | Week 8 |
| Phase 5 | Games run at playable FPS | Week 9 |

## Rollout Strategy

1. **Implement in feature branch** (`ui-layer`)
2. **Land Phase 1 ASAP** to unblock testing
3. **Expand incrementally** based on program requirements
4. **Leverage existing UI stubs** - they already handle message loops!

## Risk Mitigation

**Risks:**
- Metal API complexity → **Use wgpu abstraction**
- Handle ID collisions → **Use typed handles**
- Performance → **Profile incrementally, optimize later**
- Compatibility → **Test against real Windows programs immediately**

**Contingency:**
- If native Metal too hard → Fall back to OpenGL (glow)
- If full GDI impossible → Start with common subset (text, lines, rectangles)

---

*Last Updated: October 15, 2026*
*Status: Ready to implement Phase 1*
