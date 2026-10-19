//! Example: Rotating 3D Triangle using Direct3D 9 API
//!
//! This example demonstrates:
//! - Creating a D3D9 device
//! - Creating a vertex buffer with triangle data
//! - Basic transform matrix setup
//! - Simple rendering loop
//!
//! This would run on macOS with Metal backend translation in Phase 4.

use anyos_dx9::{
    D3DDevice, D3DDeviceHandle,
    buffers::{VertexBufferDesc, VertexBufferHandle},
    types::{D3DVERTEX_COLORED, D3DMATRIX, make_d3dcolor_rgb},
    D3DPRIMITIVETYPE, D3DTRANSFORMSTATETYPE, D3DRENDERSTATETYPE,
};
use std::f32::consts::PI;

fn main() {
    println!("AnyOS Phase 4: Direct3D 9 Triangle Example");
    println!("============================================\n");

    // Create D3D device
    let handle = D3DDeviceHandle(1);
    let mut device = D3DDevice::new(handle);
    println!("[+] Created D3D9 device");

    // Create vertex buffer for triangle
    let vb_desc = VertexBufferDesc::new(3, std::mem::size_of::<D3DVERTEX_COLORED>() as u32);
    let vb_handle = device.create_vertex_buffer(vb_desc)
        .expect("Failed to create vertex buffer");
    println!("[+] Created vertex buffer with {} bytes", std::mem::size_of::<D3DVERTEX_COLORED>() as u32 * 3);

    // Fill vertex buffer with triangle data
    {
        let vb = device.get_vertex_buffer_mut(vb_handle).expect("Failed to get vertex buffer");

        // Define triangle vertices (counter-clockwise when viewed from front)
        let vertices = [
            // Top vertex - Red
            D3DVERTEX_COLORED::new(-0.5, 0.5, 0.0, make_d3dcolor_rgb(255, 0, 0)),
            // Bottom left - Green
            D3DVERTEX_COLORED::new(-0.5, -0.5, 0.0, make_d3dcolor_rgb(0, 255, 0)),
            // Bottom right - Blue
            D3DVERTEX_COLORED::new(0.5, -0.5, 0.0, make_d3dcolor_rgb(0, 0, 255)),
        ];

        let vertex_data = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr() as *const u8,
                std::mem::size_of_val(&vertices),
            )
        };

        vb.write_data(0, vertex_data).expect("Failed to write vertex data");
        println!("[+] Loaded triangle vertex data:");
        println!("    Vertex 0: ({:+.1}, {:+.1}, 0.0) - Red",    -0.5,  0.5);
        println!("    Vertex 1: ({:+.1}, {:+.1}, 0.0) - Green",  -0.5, -0.5);
        println!("    Vertex 2: ({:+.1}, {:+.1}, 0.0) - Blue",    0.5, -0.5);
    }

    // Set up transformation matrices
    {
        // World matrix (identity)
        device.set_transform(
            D3DTRANSFORMSTATETYPE::D3DTS_WORLD,
            &D3DMATRIX::identity(),
        ).expect("Failed to set world matrix");

        // View matrix (identity - camera at origin looking down -Z)
        device.set_transform(
            D3DTRANSFORMSTATETYPE::D3DTS_VIEW,
            &D3DMATRIX::identity(),
        ).expect("Failed to set view matrix");

        // Projection matrix (simple orthographic for clarity)
        device.set_transform(
            D3DTRANSFORMSTATETYPE::D3DTS_PROJECTION,
            &D3DMATRIX::identity(),
        ).expect("Failed to set projection matrix");

        println!("[+] Set transformation matrices");
    }

    // Set up render state
    {
        device.set_render_state(D3DRENDERSTATETYPE::D3DRS_ZENABLE, 1)
            .expect("Failed to enable Z-buffer");
        device.set_render_state(D3DRENDERSTATETYPE::D3DRS_ZWRITEENABLE, 1)
            .expect("Failed to enable Z-write");
        device.set_render_state(D3DRENDERSTATETYPE::D3DRS_ALPHABLENDENABLE, 0)
            .expect("Failed to set blend state");
        println!("[+] Set render state (Z-buffer enabled, alpha blend disabled)");
    }

    // Set up stream source
    {
        device.set_stream_source(0, Some(vb_handle), 0, 0)
            .expect("Failed to set stream source");
        println!("[+] Set vertex buffer as active stream");
    }

    // Simulate frame rendering
    println!("\n[*] Simulating rendering loop...\n");

    for frame in 0..60 {
        let angle = (frame as f32 / 60.0) * 2.0 * PI;

        // Create rotation matrix
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        let rotation = D3DMATRIX {
            m: [
                [cos_a, -sin_a, 0.0, 0.0],
                [sin_a,  cos_a, 0.0, 0.0],
                [0.0,    0.0,   1.0, 0.0],
                [0.0,    0.0,   0.0, 1.0],
            ],
        };

        device.set_transform(D3DTRANSFORMSTATETYPE::D3DTS_WORLD, &rotation)
            .expect("Failed to set world matrix");

        // Begin scene
        device.begin_scene().expect("Failed to begin scene");

        // Draw triangle
        device.draw_primitive(
            D3DPRIMITIVETYPE::D3DPT_TRIANGLELIST,
            0,  // start vertex
            1,  // primitive count (1 triangle)
        ).expect("Failed to draw primitive");

        // End scene
        device.end_scene().expect("Failed to end scene");

        // Present
        device.present().expect("Failed to present");

        if frame % 10 == 0 {
            println!("  Frame {}: angle = {:.2} radians", frame, angle);
        }
    }

    println!("\n[+] Rendering loop completed successfully!");

    // Display device statistics
    let stats = device.get_stats();
    println!("\n[*] Device Statistics:");
    println!("    Vertex Buffers: {}", stats.vertex_buffers);
    println!("    Index Buffers: {}", stats.index_buffers);
    println!("    Textures: {}", stats.textures);
    println!("    Vertex Shaders: {}", stats.vertex_shaders);
    println!("    Pixel Shaders: {}", stats.pixel_shaders);

    println!("\n[SUCCESS] Phase 4 example completed!");
    println!("\nNote: In a real scenario, this would:");
    println!("  - Render to an actual macOS window via Metal");
    println!("  - Display a rotating colored triangle");
    println!("  - Run at 60+ FPS");
    println!("  - Support complex shaders and textures");
}
