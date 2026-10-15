//! Direct3D 9 Device implementation
//!
//! Core device that manages resources, render state, and rendering operations.

use crate::{
    buffers::{
        IndexBuffer, IndexBufferDesc, IndexBufferHandle, IndexFormat, VertexBuffer,
        VertexBufferDesc, VertexBufferHandle,
    },
    shaders::{
        PixelShader, PixelShaderConstants, PixelShaderHandle, VertexShader, VertexShaderConstants,
        VertexShaderHandle,
    },
    textures::{Texture, TextureDesc, TextureHandle},
    types::D3DMATRIX,
    D3DError, D3DResult, D3DBLEND, D3DCMPFUNC, D3DCULL, D3DFILLMODE, D3DPRIMITIVETYPE,
    D3DRENDERSTATETYPE, D3DSAMPLERSTATETYPE, D3DTEXTUREFILTERTYPE, D3DTRANSFORMSTATETYPE,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Handle to a graphics device
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct D3DDeviceHandle(pub u32);

/// Render state tracking
#[derive(Debug, Clone)]
struct RenderState {
    render_states: HashMap<D3DRENDERSTATETYPE, u32>,
    sampler_states: HashMap<(u32, D3DSAMPLERSTATETYPE), u32>,
    blend_enable: bool,
    src_blend: D3DBLEND,
    dst_blend: D3DBLEND,
    z_enable: bool,
    z_func: D3DCMPFUNC,
    z_write: bool,
    cull_mode: D3DCULL,
    fill_mode: D3DFILLMODE,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            render_states: HashMap::new(),
            sampler_states: HashMap::new(),
            blend_enable: false,
            src_blend: D3DBLEND::D3DBLEND_SRCALPHA,
            dst_blend: D3DBLEND::D3DBLEND_INVSRCALPHA,
            z_enable: true,
            z_func: D3DCMPFUNC::D3DCMP_LESSEQUAL,
            z_write: true,
            cull_mode: D3DCULL::D3DCULL_CCW,
            fill_mode: D3DFILLMODE::D3DFILL_SOLID,
        }
    }
}

/// Direct3D 9 Device
pub struct D3DDevice {
    pub handle: D3DDeviceHandle,

    // Resource management
    vertex_buffers: HashMap<VertexBufferHandle, VertexBuffer>,
    index_buffers: HashMap<IndexBufferHandle, IndexBuffer>,
    textures: HashMap<TextureHandle, Texture>,
    vertex_shaders: HashMap<VertexShaderHandle, VertexShader>,
    pixel_shaders: HashMap<PixelShaderHandle, PixelShader>,

    // Next resource IDs
    next_vb_id: u32,
    next_ib_id: u32,
    next_tex_id: u32,
    next_vs_id: u32,
    next_ps_id: u32,

    // Current state
    render_state: RenderState,
    transform_matrices: HashMap<D3DTRANSFORMSTATETYPE, D3DMATRIX>,
    current_vertex_shader: Option<VertexShaderHandle>,
    current_pixel_shader: Option<PixelShaderHandle>,
    vertex_shader_constants: VertexShaderConstants,
    pixel_shader_constants: PixelShaderConstants,

    // Rendering state
    in_scene: bool,
    active_vertex_buffer: Option<VertexBufferHandle>,
    active_index_buffer: Option<IndexBufferHandle>,

    // Platform-specific data
    platform_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl D3DDevice {
    /// Create a new D3D device
    pub fn new(handle: D3DDeviceHandle) -> Self {
        let mut device = Self {
            handle,
            vertex_buffers: HashMap::new(),
            index_buffers: HashMap::new(),
            textures: HashMap::new(),
            vertex_shaders: HashMap::new(),
            pixel_shaders: HashMap::new(),
            next_vb_id: 1,
            next_ib_id: 1,
            next_tex_id: 1,
            next_vs_id: 1,
            next_ps_id: 1,
            render_state: RenderState::default(),
            transform_matrices: HashMap::new(),
            current_vertex_shader: None,
            current_pixel_shader: None,
            vertex_shader_constants: VertexShaderConstants::new(256),
            pixel_shader_constants: PixelShaderConstants::new(256),
            in_scene: false,
            active_vertex_buffer: None,
            active_index_buffer: None,
            platform_data: None,
        };

        // Set default matrices
        device
            .transform_matrices
            .insert(D3DTRANSFORMSTATETYPE::D3DTS_WORLD, D3DMATRIX::identity());
        device
            .transform_matrices
            .insert(D3DTRANSFORMSTATETYPE::D3DTS_VIEW, D3DMATRIX::identity());
        device.transform_matrices.insert(
            D3DTRANSFORMSTATETYPE::D3DTS_PROJECTION,
            D3DMATRIX::identity(),
        );

        device
    }

    // Vertex buffer management

    pub fn create_vertex_buffer(
        &mut self,
        desc: VertexBufferDesc,
    ) -> D3DResult<VertexBufferHandle> {
        let handle = VertexBufferHandle(self.next_vb_id);
        self.next_vb_id += 1;

        let buffer = VertexBuffer::new(handle, desc);
        self.vertex_buffers.insert(handle, buffer);

        Ok(handle)
    }

    pub fn get_vertex_buffer(&self, handle: VertexBufferHandle) -> D3DResult<&VertexBuffer> {
        self.vertex_buffers
            .get(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Vertex buffer {:?} not found", handle)))
    }

    pub fn get_vertex_buffer_mut(
        &mut self,
        handle: VertexBufferHandle,
    ) -> D3DResult<&mut VertexBuffer> {
        self.vertex_buffers
            .get_mut(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Vertex buffer {:?} not found", handle)))
    }

    // Index buffer management

    pub fn create_index_buffer(&mut self, desc: IndexBufferDesc) -> D3DResult<IndexBufferHandle> {
        let handle = IndexBufferHandle(self.next_ib_id);
        self.next_ib_id += 1;

        let buffer = IndexBuffer::new(handle, desc);
        self.index_buffers.insert(handle, buffer);

        Ok(handle)
    }

    pub fn get_index_buffer(&self, handle: IndexBufferHandle) -> D3DResult<&IndexBuffer> {
        self.index_buffers
            .get(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Index buffer {:?} not found", handle)))
    }

    pub fn get_index_buffer_mut(
        &mut self,
        handle: IndexBufferHandle,
    ) -> D3DResult<&mut IndexBuffer> {
        self.index_buffers
            .get_mut(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Index buffer {:?} not found", handle)))
    }

    // Texture management

    pub fn create_texture(&mut self, desc: TextureDesc) -> D3DResult<TextureHandle> {
        let handle = TextureHandle(self.next_tex_id);
        self.next_tex_id += 1;

        let texture = Texture::new(handle, desc);
        self.textures.insert(handle, texture);

        Ok(handle)
    }

    pub fn get_texture(&self, handle: TextureHandle) -> D3DResult<&Texture> {
        self.textures
            .get(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Texture {:?} not found", handle)))
    }

    pub fn get_texture_mut(&mut self, handle: TextureHandle) -> D3DResult<&mut Texture> {
        self.textures
            .get_mut(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Texture {:?} not found", handle)))
    }

    // Shader management

    pub fn create_vertex_shader(&mut self, code: Vec<u8>) -> D3DResult<VertexShaderHandle> {
        let handle = VertexShaderHandle(self.next_vs_id);
        self.next_vs_id += 1;

        let shader = VertexShader::new(handle, code);
        self.vertex_shaders.insert(handle, shader);

        Ok(handle)
    }

    pub fn get_vertex_shader(&self, handle: VertexShaderHandle) -> D3DResult<&VertexShader> {
        self.vertex_shaders
            .get(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Vertex shader {:?} not found", handle)))
    }

    pub fn create_pixel_shader(&mut self, code: Vec<u8>) -> D3DResult<PixelShaderHandle> {
        let handle = PixelShaderHandle(self.next_ps_id);
        self.next_ps_id += 1;

        let shader = PixelShader::new(handle, code);
        self.pixel_shaders.insert(handle, shader);

        Ok(handle)
    }

    pub fn get_pixel_shader(&self, handle: PixelShaderHandle) -> D3DResult<&PixelShader> {
        self.pixel_shaders
            .get(&handle)
            .ok_or_else(|| D3DError::InvalidHandle(format!("Pixel shader {:?} not found", handle)))
    }

    // Render state management

    pub fn set_render_state(&mut self, state: D3DRENDERSTATETYPE, value: u32) -> D3DResult<()> {
        self.render_state.render_states.insert(state, value);
        Ok(())
    }

    pub fn get_render_state(&self, state: D3DRENDERSTATETYPE) -> D3DResult<u32> {
        Ok(*self.render_state.render_states.get(&state).unwrap_or(&0))
    }

    pub fn set_sampler_state(
        &mut self,
        sampler: u32,
        state: D3DSAMPLERSTATETYPE,
        value: u32,
    ) -> D3DResult<()> {
        self.render_state
            .sampler_states
            .insert((sampler, state), value);
        Ok(())
    }

    pub fn get_sampler_state(&self, sampler: u32, state: D3DSAMPLERSTATETYPE) -> D3DResult<u32> {
        Ok(*self
            .render_state
            .sampler_states
            .get(&(sampler, state))
            .unwrap_or(&0))
    }

    // Transformation matrices

    pub fn set_transform(
        &mut self,
        state: D3DTRANSFORMSTATETYPE,
        matrix: &D3DMATRIX,
    ) -> D3DResult<()> {
        self.transform_matrices.insert(state, *matrix);
        Ok(())
    }

    pub fn get_transform(&self, state: D3DTRANSFORMSTATETYPE) -> D3DResult<D3DMATRIX> {
        Ok(*self
            .transform_matrices
            .get(&state)
            .unwrap_or(&D3DMATRIX::identity()))
    }

    // Shader constants

    pub fn set_vertex_shader_constant_f(
        &mut self,
        start_register: u32,
        constant: &[f32],
    ) -> D3DResult<()> {
        self.vertex_shader_constants
            .set_vector(start_register as usize, constant)?;
        Ok(())
    }

    pub fn get_vertex_shader_constants(&self) -> &[f32] {
        self.vertex_shader_constants.get_data()
    }

    pub fn set_pixel_shader_constant_f(
        &mut self,
        start_register: u32,
        constant: &[f32],
    ) -> D3DResult<()> {
        self.pixel_shader_constants
            .set_vector(start_register as usize, constant)?;
        Ok(())
    }

    pub fn get_pixel_shader_constants(&self) -> &[f32] {
        self.pixel_shader_constants.get_data()
    }

    // Current shader management

    pub fn set_vertex_shader(&mut self, handle: Option<VertexShaderHandle>) -> D3DResult<()> {
        if let Some(h) = handle {
            let _ = self.get_vertex_shader(h)?;
        }
        self.current_vertex_shader = handle;
        Ok(())
    }

    pub fn get_vertex_shader_handle(&self) -> Option<VertexShaderHandle> {
        self.current_vertex_shader
    }

    pub fn set_pixel_shader(&mut self, handle: Option<PixelShaderHandle>) -> D3DResult<()> {
        if let Some(h) = handle {
            let _ = self.get_pixel_shader(h)?;
        }
        self.current_pixel_shader = handle;
        Ok(())
    }

    pub fn get_pixel_shader_handle(&self) -> Option<PixelShaderHandle> {
        self.current_pixel_shader
    }

    // Stream management

    pub fn set_stream_source(
        &mut self,
        stream_number: u32,
        buffer_handle: Option<VertexBufferHandle>,
        offset: u32,
        stride: u32,
    ) -> D3DResult<()> {
        if let Some(handle) = buffer_handle {
            let buffer = self.get_vertex_buffer(handle)?;
            if stride != buffer.get_stride() && stride != 0 {
                return Err(D3DError::StateError(format!(
                    "Stream stride {} doesn't match buffer stride {}",
                    stride,
                    buffer.get_stride()
                )));
            }
        }

        if stream_number == 0 {
            self.active_vertex_buffer = buffer_handle;
        }

        Ok(())
    }

    pub fn set_indices(&mut self, buffer_handle: Option<IndexBufferHandle>) -> D3DResult<()> {
        if let Some(handle) = buffer_handle {
            let _ = self.get_index_buffer(handle)?;
        }
        self.active_index_buffer = buffer_handle;
        Ok(())
    }

    // Scene management

    pub fn begin_scene(&mut self) -> D3DResult<()> {
        if self.in_scene {
            return Err(D3DError::RenderError(
                "Scene already in progress".to_string(),
            ));
        }
        self.in_scene = true;
        Ok(())
    }

    pub fn end_scene(&mut self) -> D3DResult<()> {
        if !self.in_scene {
            return Err(D3DError::RenderError("No scene in progress".to_string()));
        }
        self.in_scene = false;
        Ok(())
    }

    pub fn is_in_scene(&self) -> bool {
        self.in_scene
    }

    // Rendering

    pub fn draw_primitive(
        &self,
        primitive_type: D3DPRIMITIVETYPE,
        start_vertex: u32,
        primitive_count: u32,
    ) -> D3DResult<()> {
        if !self.in_scene {
            return Err(D3DError::RenderError("BeginScene not called".to_string()));
        }

        if self.active_vertex_buffer.is_none() {
            return Err(D3DError::RenderError("No active vertex buffer".to_string()));
        }

        // Validation: ensure we have enough vertices
        let vb = self.get_vertex_buffer(self.active_vertex_buffer.unwrap())?;
        let vertex_count = match primitive_type {
            D3DPRIMITIVETYPE::D3DPT_POINTLIST => primitive_count,
            D3DPRIMITIVETYPE::D3DPT_LINELIST => primitive_count * 2,
            D3DPRIMITIVETYPE::D3DPT_LINESTRIP => primitive_count + 1,
            D3DPRIMITIVETYPE::D3DPT_TRIANGLELIST => primitive_count * 3,
            D3DPRIMITIVETYPE::D3DPT_TRIANGLESTRIP => primitive_count + 2,
            D3DPRIMITIVETYPE::D3DPT_TRIANGLEFAN => primitive_count + 2,
        };

        if start_vertex + vertex_count > vb.get_vertex_count() {
            return Err(D3DError::RenderError(format!(
                "Not enough vertices: need {}, have {}",
                start_vertex + vertex_count,
                vb.get_vertex_count()
            )));
        }

        Ok(())
    }

    pub fn draw_indexed_primitive(
        &self,
        primitive_type: D3DPRIMITIVETYPE,
        base_vertex_index: i32,
        min_vertex_index: u32,
        num_vertices: u32,
        start_index: u32,
        primitive_count: u32,
    ) -> D3DResult<()> {
        if !self.in_scene {
            return Err(D3DError::RenderError("BeginScene not called".to_string()));
        }

        if self.active_vertex_buffer.is_none() {
            return Err(D3DError::RenderError("No active vertex buffer".to_string()));
        }

        if self.active_index_buffer.is_none() {
            return Err(D3DError::RenderError("No active index buffer".to_string()));
        }

        // Index count validation
        let index_count = match primitive_type {
            D3DPRIMITIVETYPE::D3DPT_POINTLIST => primitive_count,
            D3DPRIMITIVETYPE::D3DPT_LINELIST => primitive_count * 2,
            D3DPRIMITIVETYPE::D3DPT_LINESTRIP => primitive_count + 1,
            D3DPRIMITIVETYPE::D3DPT_TRIANGLELIST => primitive_count * 3,
            D3DPRIMITIVETYPE::D3DPT_TRIANGLESTRIP => primitive_count + 2,
            D3DPRIMITIVETYPE::D3DPT_TRIANGLEFAN => primitive_count + 2,
        };

        let ib = self.get_index_buffer(self.active_index_buffer.unwrap())?;
        if start_index + index_count > ib.get_index_count() {
            return Err(D3DError::RenderError(format!(
                "Not enough indices: need {}, have {}",
                start_index + index_count,
                ib.get_index_count()
            )));
        }

        Ok(())
    }

    /// Clear the render targets
    pub fn clear(&self, flags: u32, color: u32, _z: f32, _stencil: u32) -> D3DResult<()> {
        // This would actually clear the render targets in the backend
        // For now, just validate the call
        if !self.in_scene {
            return Err(D3DError::RenderError(
                "Cannot clear outside scene".to_string(),
            ));
        }
        Ok(())
    }

    /// Present the frame
    pub fn present(&self) -> D3DResult<()> {
        if self.in_scene {
            return Err(D3DError::RenderError(
                "Cannot present while scene in progress".to_string(),
            ));
        }
        Ok(())
    }

    /// Get device statistics
    pub fn get_stats(&self) -> DeviceStats {
        DeviceStats {
            vertex_buffers: self.vertex_buffers.len() as u32,
            index_buffers: self.index_buffers.len() as u32,
            textures: self.textures.len() as u32,
            vertex_shaders: self.vertex_shaders.len() as u32,
            pixel_shaders: self.pixel_shaders.len() as u32,
        }
    }
}

/// Device statistics
#[derive(Debug, Clone)]
pub struct DeviceStats {
    pub vertex_buffers: u32,
    pub index_buffers: u32,
    pub textures: u32,
    pub vertex_shaders: u32,
    pub pixel_shaders: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_creation() {
        let handle = D3DDeviceHandle(1);
        let device = D3DDevice::new(handle);

        assert_eq!(device.handle, handle);
        assert!(!device.in_scene);
        assert!(device.current_vertex_shader.is_none());
    }

    #[test]
    fn test_vertex_buffer_creation() {
        let handle = D3DDeviceHandle(1);
        let mut device = D3DDevice::new(handle);

        let desc = VertexBufferDesc::new(100, 32);
        let vb_handle = device.create_vertex_buffer(desc).unwrap();

        assert_eq!(vb_handle.0, 1);
        let _ = device.get_vertex_buffer(vb_handle).unwrap();
    }

    #[test]
    fn test_scene_management() {
        let handle = D3DDeviceHandle(1);
        let mut device = D3DDevice::new(handle);

        assert!(!device.is_in_scene());
        device.begin_scene().unwrap();
        assert!(device.is_in_scene());
        device.end_scene().unwrap();
        assert!(!device.is_in_scene());
    }

    #[test]
    fn test_render_state() {
        let handle = D3DDeviceHandle(1);
        let mut device = D3DDevice::new(handle);

        device
            .set_render_state(D3DRENDERSTATETYPE::D3DRS_ZENABLE, 1)
            .unwrap();
        assert_eq!(
            device
                .get_render_state(D3DRENDERSTATETYPE::D3DRS_ZENABLE)
                .unwrap(),
            1
        );
    }

    #[test]
    fn test_device_stats() {
        let handle = D3DDeviceHandle(1);
        let mut device = D3DDevice::new(handle);

        let _ = device.create_vertex_buffer(VertexBufferDesc::new(100, 32));
        let _ = device.create_texture(TextureDesc::new(
            256,
            256,
            crate::D3DFORMAT::D3DFMT_A8R8G8B8,
        ));

        let stats = device.get_stats();
        assert_eq!(stats.vertex_buffers, 1);
        assert_eq!(stats.textures, 1);
    }
}
