//! Drawing test example
//!
//! Demonstrates Phase 2 graphics capabilities:
//! - Text rendering
//! - Line drawing
//! - Rectangle drawing
//! - Filled rectangles
//! - Pixel setting

use anyos_ui::{
    graphics::{Color, Rect},
    window::WindowParams,
    UIBackend,
};

#[cfg(target_os = "macos")]
use anyos_ui::platform::macos::MacOSBackend as Backend;

#[cfg(target_os = "windows")]
use anyos_ui::platform::windows::WindowsBackend as Backend;

#[cfg(target_os = "linux")]
use anyos_ui::platform::linux::LinuxBackend as Backend;

fn main() -> anyos_ui::UIResult<()> {
    println!("AnyOS Graphics Test - Phase 2");
    println!("==============================\n");

    // Create backend
    let mut backend = Backend::new()?;
    println!("Backend initialized");

    // Create a test window
    let params = WindowParams {
        title: "AnyOS Graphics Test".to_string(),
        rect: Rect::new(100, 100, 800, 600),
        style: 0x10000000, // WS_VISIBLE
        ..WindowParams::default()
    };

    let window = backend.create_window(params)?;
    println!("Window created: {:?}", window);

    // Create a device context for drawing
    let dc = backend.create_dc(window)?;
    println!("Device context created: 0x{:x}", dc);

    println!("\nDrawing operations:");
    println!("-------------------");

    // Draw a filled background rectangle
    backend.fill_rect(dc, Rect::new(10, 10, 790, 590), Color::WHITE)?;
    println!("1. Filled background white rectangle");

    // Draw a bordered rectangle
    backend.rectangle(dc, Rect::new(50, 50, 750, 550), false)?;
    println!("2. Rectangle outline");

    // Draw some lines
    backend.line_to(dc, 50, 50, 750, 550)?;
    println!("3. Diagonal line (top-left to bottom-right)");

    backend.line_to(dc, 750, 50, 50, 550)?;
    println!("4. Diagonal line (top-right to bottom-left)");

    // Draw a filled rectangle in the center
    backend.fill_rect(dc, Rect::new(300, 200, 500, 400), Color::new(100, 150, 200))?;
    println!("5. Filled blue rectangle in center");

    // Draw text
    backend.text_out(dc, 60, 70, "Hello, AnyOS Graphics!", Color::BLACK)?;
    println!("6. Text: 'Hello, AnyOS Graphics!'");

    backend.text_out(
        dc,
        60,
        100,
        "This is a graphics demonstration",
        Color::new(200, 0, 0),
    )?;
    println!("7. Text: 'This is a graphics demonstration'");

    // Draw some pixel art
    println!("8. Pixel grid pattern");
    for i in 0..10 {
        for j in 0..10 {
            backend.set_pixel(dc, 100 + i * 2, 300 + j * 2, Color::RED)?;
        }
    }

    // Draw a red rectangle with a title
    backend.rectangle(dc, Rect::new(550, 80, 750, 150), false)?;
    backend.text_out(dc, 560, 90, "Info Box", Color::new(150, 0, 0))?;
    println!("9. Info box with title");

    println!("\nDrawing complete!");
    println!("\nOperations recorded in drawing state.");
    println!("On macOS, these would be rendered when the window's drawRect: is called.");

    // Keep window alive briefly
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Cleanup
    backend.destroy_dc(dc)?;
    backend.destroy_window(window)?;
    println!("\nResources cleaned up.");

    Ok(())
}
