# DirectX 9 Implementation Architecture (Phase 4)

## Overview

Phase 4 adds 3D graphics support to AnyOS by implementing a subset of the DirectX 9 API. Games running under AnyOS can now use D3D9 for 3D rendering, which is then translated to native platform graphics APIs (Metal on macOS, Vulkan on Linux, native Direct3D on Windows).

## Core Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                  Win32 Game Application                          │
│              (calling DirectX 9 API functions)                   │
└──────────────────────────────┬──────────────────────────────────┘
                                │
                    ┌───────────▼─────────────┐
                    │   anyos-dx9 Crate       │
                    │  (D3D API Layer)        │
                    │                         │
                    │ - D3DDevice             │
                    │ - VertexBuffer          │
                    │ - IndexBuffer           │
                    │ - Texture               │
                    │ - VertexShader          │
                    │ - PixelShader           │
                    │ - Render State          │
                    └───────────┬─────────────┘
                                │
                ┌───────────────┼───────────────┐
                │               │               │
         ┌──────▼────────┐ ┌────▼────────┐ ┌───▼──────────┐
         │  macOS Metal  │ │  Vulkan     │ │  Direct3D    │
         │  Backend      │ │  Backend    │ │  Backend     │
         │               │ │             │ │              │
         │ MetalDevice   │ │ VkDevice    │ │ IDirect3D9   │
         │ MTLBuffer     │ │ VkBuffer    │ │              │
         │ MTLTexture    │ │ VkImage     │ │              │
         │ RenderPass    │ │ RenderPass  │ │              │
         └────┬──────────┘ └──┬──────────┘ └────┬─────────┘
              │                │                │
              └────────────────┼────────────────┘
                              │
                    ┌─────────▼──────────┐
                    │  Native Graphics   │
                    │  Rendered Output   │
                    └────────────────────┘
```

## Module Structure

### 1. **types.rs** - Core Data Structures
Defines the fundamental types used throughout the D3D9 implementation:

- **D3DMATRIX** - 4x4 transformation matrices for world/view/projection
- **D3DVECTOR** - 3D vector with normal
- **D3DVERTEX** - Standard vertex (position, normal, texture coords)
- **D3DVERTEX_COLORED** - Colored vertex (for simple rendering)
- **D3DTLVERTEX** - Pre-transformed vertex (screen space)
- Color and index format utilities
- Enumeration types for sampler filters, address modes, pools

### 2. **buffers.rs** - Vertex and Index Buffers
Manages GPU memory for geometry:

```rust
pub struct VertexBuffer {
    handle: VertexBufferHandle,
    desc: VertexBufferDesc,
    data: Vec<u8>,        // CPU-side data
    locked: bool,
    platform_data: Option<Arc<dyn Any>>,  // Platform-specific (MTLBuffer, etc.)
}

pub struct IndexBuffer {
    handle: IndexBufferHandle,
    desc: IndexBufferDesc,
    data: Vec<u8>,
    locked: bool,
    platform_data: Option<Arc<dyn Any>>,
}
```

Key operations:
- Lock/Unlock for writing data
- Format validation (16-bit vs 32-bit indices)
- Stride validation

### 3. **textures.rs** - Texture Management
Handles texture resources and mipmap chains:

```rust
pub struct Texture {
    handle: TextureHandle,
    desc: TextureDesc,
    surfaces: Vec<TextureSurface>,  // Mipmap levels
    platform_data: Option<Arc<dyn Any>>,
}
```

Supported formats:
- A8R8G8B8, X8R8G8B8 (32-bit ARGB)
- R8G8B8 (24-bit RGB)
- R5G6B5, A1R5G5B5 (16-bit)
- DXT1/DXT3/DXT5 (compressed)

### 4. **shaders.rs** - Shader Compilation and Constants
Manages vertex and pixel shaders:

```rust
pub struct VertexShader {
    handle: VertexShaderHandle,
    code: Vec<u8>,           // HLSL bytecode or source
    platform_data: Option<Arc<dyn Any>>,  // Compiled Metal/GLSL
}

pub struct VertexShaderConstants {
    data: Vec<f32>,          // 4-byte register slots
    dirty: bool,             // Marks if update needed
}
```

Features:
- Shader constant buffers (registers c0-c255)
- Dirty flag tracking for optimization
- Matrix/vector/scalar constant setting

### 5. **device.rs** - Main D3D Device
Core device implementation managing all resources and rendering state:

```rust
pub struct D3DDevice {
    // Resource pools
    vertex_buffers: HashMap<VertexBufferHandle, VertexBuffer>,
    index_buffers: HashMap<IndexBufferHandle, IndexBuffer>,
    textures: HashMap<TextureHandle, Texture>,
    vertex_shaders: HashMap<VertexShaderHandle, VertexShader>,
    pixel_shaders: HashMap<PixelShaderHandle, PixelShader>,
    
    // Current state
    render_state: RenderState,
    transform_matrices: HashMap<D3DTRANSFORMSTATETYPE, D3DMATRIX>,
    current_vertex_shader: Option<VertexShaderHandle>,
    current_pixel_shader: Option<PixelShaderHandle>,
    
    // Rendering
    in_scene: bool,
    active_vertex_buffer: Option<VertexBufferHandle>,
    active_index_buffer: Option<IndexBufferHandle>,
}
```

Key methods:
- Resource creation (buffers, textures, shaders)
- Render state management (blend, depth, cull, fill modes)
- Transform matrix operations
- Shader constant updates
- Scene begin/end and drawing calls

## Rendering Pipeline

### 1. Device Creation
```rust
let mut device = D3DDevice::new(D3DDeviceHandle(1));
```

### 2. Resource Creation
```rust
let vb_handle = device.create_vertex_buffer(
    VertexBufferDesc::new(1000, 32)  // 1000 vertices, 32 bytes each
)?;

let tex_handle = device.create_texture(
    TextureDesc::new(512, 512, D3DFORMAT::D3DFMT_A8R8G8B8)
)?;

let vs_handle = device.create_vertex_shader(bytecode)?;
let ps_handle = device.create_pixel_shader(bytecode)?;
```

### 3. Data Upload
```rust
{
    let vb = device.get_vertex_buffer_mut(vb_handle)?;
    let ptr = vb.lock()?;
    // Copy data to ptr
    vb.unlock()?;
}
```

### 4. State Setup
```rust
device.set_stream_source(0, Some(vb_handle), 0, 0)?;
device.set_indices(Some(ib_handle))?;
device.set_vertex_shader(Some(vs_handle))?;
device.set_pixel_shader(Some(ps_handle))?;

device.set_render_state(D3DRS_ZENABLE, 1)?;
device.set_render_state(D3DRS_CULLMODE, D3DCULL_CCW as u32)?;

device.set_transform(D3DTS_WORLD, &world_matrix)?;
device.set_transform(D3DTS_VIEW, &view_matrix)?;
device.set_transform(D3DTS_PROJECTION, &proj_matrix)?;
```

### 5. Rendering
```rust
device.begin_scene()?;
device.draw_primitive(D3DPT_TRIANGLELIST, 0, 1000)?;
device.end_scene()?;
device.present()?;
```

## State Management

### Render State Tracking
The device maintains a complete render state including:
- Blend operations (source/destination blend)
- Depth testing and writing
- Cull mode (none, CW, CCW)
- Fill mode (point, wireframe, solid)
- Z-function (never, less, equal, always, etc.)
- Sampler states per texture unit
- Texture stage operations

### Matrix Stack
- D3DTS_WORLD - Object space to world space
- D3DTS_VIEW - World space to camera space
- D3DTS_PROJECTION - Camera space to clip space
- D3DTS_TEXTURE0-7 - Texture coordinate transformations

### Shader Constants
- Vertex shader: 256 registers (4 floats per register)
- Pixel shader: 256 registers
- Automatic updates to platform shaders when dirty

## Platform Integration (anyos-ui)

The anyos-ui crate extends UIBackend with graphics device support:

```rust
pub trait UIBackend: Send + Sync {
    // ... existing methods ...
    
    fn create_graphics_device(&mut self, hwnd: WindowHandle) -> UIResult<u64>;
    fn destroy_graphics_device(&mut self, device: u64) -> UIResult<()>;
    fn present_graphics_device(&mut self, device: u64) -> UIResult<()>;
}
```

### macOS Metal Backend
Located in `anyos-ui/src/platform/metal.rs`:

```rust
pub struct MetalGraphicsDevice {
    id: u64,
    metal_device: Arc<MetalDevice>,
    command_queue: Arc<MetalCommandQueue>,
    drawable: Option<MetalDrawable>,
}
```

The Metal backend:
1. Creates MTLDevice for GPU access
2. Wraps vertex/index buffers in MTLBuffer
3. Compiles shaders to Metal Shading Language
4. Records render commands in command buffers
5. Presents to the drawable (CAMetalLayer)

## DirectX 9 API Subset

### Implemented Functions
- `D3DCreate9` - Create D3D interface
- `CreateDevice` - Create graphics device
- `CreateVertexBuffer` - Create vertex buffer
- `CreateIndexBuffer` - Create index buffer
- `CreateTexture` - Create texture
- `CreateVertexShader` - Create vertex shader
- `CreatePixelShader` - Create pixel shader
- `SetTransform` - Set transformation matrix
- `SetRenderState` - Set render state
- `SetSamplerState` - Set texture sampler state
- `SetStreamSource` - Set active vertex buffer
- `SetIndices` - Set active index buffer
- `SetVertexShader` - Set active vertex shader
- `SetPixelShader` - Set active pixel shader
- `BeginScene` - Start frame rendering
- `EndScene` - End frame rendering
- `DrawPrimitive` - Draw vertices
- `DrawIndexedPrimitive` - Draw indexed vertices
- `Present` - Display frame

### Not Implemented (Future)
- Lighting and material support
- Multiple render targets
- Stencil buffer operations
- Advanced pixel formats
- Compressed texture loading
- Performance queries

## Performance Considerations

### Memory Management
- Buffers use CPU-side Vec<u8> for compatibility
- Platform backends convert to GPU buffers on upload
- Dirty flag tracking minimizes constant updates
- Resource handles are non-recycled 32-bit integers

### State Caching
- Render state changes are recorded but not applied until draw call
- Matrix updates marked dirty to avoid redundant uploads
- Shader constant buffers support partial updates

### Validation
- Stream source stride validation
- Vertex count validation against buffer size
- Index count validation
- Matrix handle validation

## Integration with Win32 Layer

When a game calls DirectX 9 functions:

1. Win32 thunks route D3D calls to anyos-dx9
2. Device maintains resource pool and state
3. On draw call, platform backend translates to native graphics API
4. Results render to window managed by anyos-ui

Example call flow:
```
Game: CreateDevice(...)
  ↓
Win32 Thunk: CreateDevice
  ↓
anyos-dx9: D3DDevice::create_device (creates device)
  ↓
anyos-ui: create_graphics_device (platform-specific setup)
  ↓
Metal Backend: MetalGraphicsDevice::new (allocates MTLDevice)
```

## Error Handling

All operations return `D3DResult<T>` with specific error types:
- `DeviceCreationFailed` - Cannot create device
- `BufferCreationFailed` - Cannot create buffer
- `TextureCreationFailed` - Cannot create texture
- `ShaderCompilationFailed` - Shader errors
- `RenderError` - Runtime rendering errors
- `StateError` - Invalid state operations
- `InvalidHandle` - Handle doesn't exist
- `OutOfMemory` - Allocation failure

## Testing

Each module includes unit tests:
- Buffer locking/unlocking
- Texture mipmap generation
- Shader constant updates
- Device statistics tracking
- Render state validation

Run tests with:
```bash
cargo test -p anyos-dx9
```

## Future Enhancements

### Phase 5 - Advanced Graphics
- Lighting calculations
- Multiple render targets
- Advanced texture formats
- Shader compilation pipeline
- Performance profiling

### Phase 6 - Shader Support
- HLSL to GLSL translation
- HLSL to Metal Shading Language
- Shader cache
- Constant buffer optimization

### Phase 7 - Game Compatibility
- DLL function hooking
- D3D function thunking
- Memory management hooks
- DirectInput integration
