# AnyOS: What's Next

## Current Status (October 15, 2026)

✅ **Complete:**
- Rust-based Windows PE loader
- Win32 API implementation (console + basic UI stubs)
- Cross-platform build system
- GUI application
- Installer support
- Environment/registry management
- Universal UI abstraction layer

🚧 **In Progress:**
- macOS window/graphics rendering
- Win32 API expansion
- Real UI integration

❌ **Not Yet:**
- DirectX/3D graphics
- Full GDI support
- Linux Vulkan backend
- Complex applications

## The Ask

You wanted:
1. **Expand Win32 APIs** ✓ Framework ready
2. **Create universal UI layer for all platforms** ✓ Built
3. **Make macOS version actually run .exe files** - Ready to start

## The Plan

We have two parallel tracks:

### Track 1: UI Rendering on macOS (2-9 weeks)
**Goal**: Windows programs display native macOS windows with graphics

**Phases:**
1. **Text rendering** (Weeks 1-2)
   - Cocoa window creation
   - Metal/CoreGraphics text
   - Test: Simple "Hello World" window

2. **GDI graphics** (Weeks 3-4)
   - Line drawing
   - Rectangle/shapes
   - Color/brush support
   - Test: Paint-like program

3. **Dialog support** (Week 5)
   - Dialog boxes
   - Common controls
   - Test: Setup wizards

4. **3D graphics** (Weeks 6-8)
   - DirectX 9 subset
   - GPU rendering
   - Shader support
   - Test: Simple games

5. **Optimization** (Week 9)
   - Native code execution
   - Performance tuning
   - Test: Complex games

**Files to modify:**
- `crates/anyos-ui/src/platform/macos.rs` - Main macOS implementation
- `crates/anyos-win32/src/ui.rs` - Hook into real UIBackend
- `crates/anyos-win32/src/gdi.rs` - GDI API expansion (NEW)
- `apps/anyos-gui/` - Integration with launcher

### Track 2: Win32 API Expansion (Ongoing)
**Goal**: Support more Windows APIs as programs demand them

**Priority order:**
1. **User32 (window management)** - CRITICAL
   - CreateWindowEx hooks
   - Message loop integration
   - Event handling

2. **GDI (graphics)** - CRITICAL
   - Text rendering
   - Shape drawing
   - Color management

3. **Dialogs** - IMPORTANT
   - DialogBox
   - DialogBoxIndirect
   - Common controls

4. **Input** - IMPORTANT
   - Mouse handling
   - Keyboard handling
   - GetMessage enhancement

5. **Graphics APIs** - IMPORTANT
   - DirectX subset
   - OpenGL
   - Vulkan bridging

**Files to modify:**
- `crates/anyos-win32/src/lib.rs` - API dispatcher
- `crates/anyos-win32/src/api.rs` - Add new API stubs
- New: `crates/anyos-win32/src/gdi.rs` - GDI implementation
- New: `crates/anyos-win32/src/input.rs` - Input handling
- New: `crates/anyos-win32/src/graphics.rs` - Graphics APIs

## Execution Order

**Week 1-2: Foundation**
1. Hook macOS Cocoa window creation
2. Integrate Metal/CoreGraphics
3. Wire CreateWindowEx → UIBackend
4. Test: Window appears on macOS

**Week 3-4: Drawing**
1. Implement GDI calls → Metal drawing
2. Add brush/pen/color support
3. Expand anyos-win32 GDI module
4. Test: Programs can draw

**Week 5+: Expansion**
1. Dialog support
2. Input handling
3. Graphics pipeline
4. Complex applications

## Resources

### Existing Code to Leverage
- `apps/anyos-gui/` - Cocoa/Metal integration reference
- `crates/anyos-interp/` - Instruction execution engine
- `crates/anyos-loader/` - PE loading (already works)

### New Code to Write
- `crates/anyos-ui/src/platform/macos.rs` - Finish implementation
- `crates/anyos-win32/src/gdi.rs` - GDI APIs
- `crates/anyos-win32/src/input.rs` - Keyboard/mouse
- Connection between anyos-ui and anyos-win32

### External Dependencies
- `cocoa` 0.25 - macOS Cocoa bindings
- `core-graphics` 0.23 - Metal/graphics
- `objc` 0.2 - Objective-C runtime
- (Others TBD for Linux/Windows backends)

## Key Insight: Use What You Have

Your existing code already has:
- ✅ PE loader that works
- ✅ Win32 API structure
- ✅ Cocoa integration (in GUI app)
- ✅ Message loop infrastructure
- ✅ UI abstraction layer (just built)

**We're not starting from scratch. We're connecting pieces.**

## The Bottleneck

The main blocker is **Cocoa + Metal integration in macOS backend**.

Once that works:
- Windows → HWND creation works
- UI rendering works
- Programs display on macOS
- Everything else follows

## Success Metrics

### Phase 1 (Text rendering)
- [ ] Windows program creates window on macOS
- [ ] Window displays title and text
- [ ] Close button works
- [ ] Multiple windows work

### Phase 2 (Drawing)
- [ ] Rectangles render
- [ ] Lines render
- [ ] Colors work
- [ ] Fill operations work

### Phase 3 (Dialogs)
- [ ] Dialog boxes appear
- [ ] Buttons respond
- [ ] Text input works

### Phase 4 (Graphics)
- [ ] Triangles render
- [ ] Textures display
- [ ] Shaders compile
- [ ] FPS acceptable

### Phase 5 (Games)
- [ ] Real game runs
- [ ] Performance acceptable
- [ ] No major bugs

## How to Start

### Option A: Deep Dive (Recommended)
1. Clone repo on your MacBook Pro
2. Start with Phase 1 macOS implementation
3. Use existing `apps/anyos-gui` as reference
4. Get first window to appear
5. Iteratively expand

### Option B: Parallel Work
1. Task 1: You work on macOS UI rendering
2. Task 2: Simultaneously expand Win32 APIs
3. Task 3: Test with simple programs
4. Merge results

### Option C: Guided Implementation
- I can provide step-by-step implementation for macOS
- You take ownership and iterate
- We debug together on your Mac

## Tools Needed

**On macOS:**
```bash
# Already required
- Rust (already installed via rustup)
- Xcode Command Line Tools

# Already in project
- cargo
- Swift Package Manager (for launcher)

# Optional but helpful
- Xcode.app (for debugging)
- Instruments (for profiling)
```

**Already available:**
- Full Rust toolchain
- All Cargo dependencies in Cargo.toml
- Build system ready

## Next Session

When you're ready:
1. Clone your AnyOS repo on your MacBook Pro
2. Run: `cargo build --release`
3. Start Phase 1: Implement `MacOSBackend::create_window()`
4. Use `apps/anyos-gui/macos/` as reference
5. Get first window to render

Or tell me:
- What you want to focus on first
- If you want me to drive the implementation
- If you want to take the wheel

---

## The Big Picture

**What we're doing**: Making Windows software run natively on macOS with real UIs, then expanding to other OSes.

**Why it matters**: Software freedom—programs stop being locked to one OS.

**Timeline**: 
- Phase 1 (working macOS windows): 2 weeks
- Full feature parity with real Windows: 9-10 weeks
- Beyond (Linux, other OSes): Ongoing

**Current blocker**: Cocoa/Metal integration
**Current opportunity**: Everything else is ready to go

You have the foundation. Time to build.

---

*Last updated: October 15, 2026*
*Status: Ready for Phase 1 implementation*
