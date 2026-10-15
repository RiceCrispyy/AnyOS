//! Graphics optimization for Metal rendering pipeline
//!
//! Implements:
//! - Command buffer batching
//! - Texture atlasing
//! - Shader caching
//! - Render state change minimization
//! - GPU upload scheduling

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Cached render state to minimize GPU state changes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderState {
    /// Blend mode
    pub blend_mode: u32,

    /// Depth test enabled
    pub depth_test: bool,

    /// Cull mode
    pub cull_mode: u32,

    /// Scissor rect enabled
    pub scissor_enabled: bool,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            blend_mode: 0,
            depth_test: true,
            cull_mode: 1, // Back culling
            scissor_enabled: false,
        }
    }
}

/// Texture atlas for batch texture uploads
pub struct TextureAtlas {
    /// Texture atlas size (512x512, 1024x1024, etc.)
    pub size: u32,

    /// Texture data
    pub data: Vec<u8>,

    /// Texture entries: region_id -> (x, y, w, h)
    pub regions: Arc<Mutex<HashMap<u32, (u32, u32, u32, u32)>>>,

    /// Next available offset
    pub next_offset: Arc<Mutex<u32>>,
}

impl TextureAtlas {
    pub fn new(size: u32) -> Self {
        let byte_size = (size * size * 4) as usize; // RGBA

        Self {
            size,
            data: vec![0; byte_size],
            regions: Arc::new(Mutex::new(HashMap::new())),
            next_offset: Arc::new(Mutex::new(0)),
        }
    }

    /// Add a texture region to the atlas
    pub fn add_region(&self, region_id: u32, texture_data: &[u8], width: u32, height: u32) -> Option<(u32, u32)> {
        let mut offset = self.next_offset.lock().unwrap();
        let byte_size = (width * height * 4) as usize;

        // Simple row-packing layout
        let x = (*offset % (self.size / width)) * width;
        let y = (*offset / (self.size / width)) * height;

        if y + height > self.size {
            return None; // Atlas is full
        }

        // Copy texture data
        let start = ((y * self.size + x) * 4) as usize;
        if start + byte_size <= self.data.len() {
            self.data[start..start + byte_size].copy_from_slice(texture_data);

            let mut regions = self.regions.lock().unwrap();
            regions.insert(region_id, (x, y, width, height));

            *offset += (width * height);
            Some((x, y))
        } else {
            None
        }
    }

    /// Get the position of a region in the atlas
    pub fn get_region(&self, region_id: u32) -> Option<(u32, u32, u32, u32)> {
        self.regions.lock().unwrap().get(&region_id).copied()
    }
}

/// Shader cache for compiled Metal shaders
pub struct ShaderCache {
    /// Cached shaders: source_hash -> compiled_data
    shaders: Arc<Mutex<HashMap<u64, Vec<u8>>>>,

    /// Shader source hashes
    hashes: Arc<Mutex<HashMap<String, u64>>>,
}

impl ShaderCache {
    pub fn new() -> Self {
        Self {
            shaders: Arc::new(Mutex::new(HashMap::new())),
            hashes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or compile a shader
    pub fn get_or_compile(&self, source: &str, name: &str) -> Result<Vec<u8>, String> {
        // Compute hash of source
        let hash = Self::hash_source(source);

        // Check cache
        {
            let shaders = self.shaders.lock().unwrap();
            if let Some(compiled) = shaders.get(&hash) {
                return Ok(compiled.clone());
            }
        }

        // Compile shader (placeholder - in production would use Metal compiler)
        let compiled = self.compile_metal_shader(source, name)?;

        // Cache result
        {
            let mut shaders = self.shaders.lock().unwrap();
            shaders.insert(hash, compiled.clone());
        }

        Ok(compiled)
    }

    /// Hash shader source
    fn hash_source(source: &str) -> u64 {
        // Simple hash function (production would use proper hasher)
        source.bytes().fold(0u64, |acc, byte| {
            acc.wrapping_mul(31).wrapping_add(byte as u64)
        })
    }

    /// Compile Metal shader (placeholder)
    fn compile_metal_shader(&self, _source: &str, _name: &str) -> Result<Vec<u8>, String> {
        // In production, this would invoke the Metal shader compiler
        // For now, return a placeholder
        Ok(vec![0u8; 256])
    }

    /// Clear the cache
    pub fn clear(&self) {
        self.shaders.lock().unwrap().clear();
        self.hashes.lock().unwrap().clear();
    }
}

impl Default for ShaderCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Command buffer optimizer for Metal rendering
pub struct CommandBufferOptimizer {
    /// Cached render state
    current_state: Arc<Mutex<RenderState>>,

    /// Command batches pending upload
    pending_batches: Arc<Mutex<Vec<RenderBatch>>>,

    /// State change count (for statistics)
    state_changes: Arc<Mutex<u64>>,
}

/// A batch of render commands
#[derive(Debug, Clone)]
pub struct RenderBatch {
    /// Batch ID
    pub id: u32,

    /// Render state for this batch
    pub state: RenderState,

    /// Vertex buffer start offset
    pub vertex_offset: u32,

    /// Index count
    pub index_count: u32,

    /// Texture region ID
    pub texture_id: u32,
}

impl CommandBufferOptimizer {
    pub fn new() -> Self {
        Self {
            current_state: Arc::new(Mutex::new(RenderState::default())),
            pending_batches: Arc::new(Mutex::new(Vec::new())),
            state_changes: Arc::new(Mutex::new(0)),
        }
    }

    /// Add a render batch, optimizing state changes
    pub fn add_batch(&self, batch: RenderBatch) {
        let mut current = self.current_state.lock().unwrap();

        if batch.state != *current {
            *self.state_changes.lock().unwrap() += 1;
            *current = batch.state;
        }

        self.pending_batches.lock().unwrap().push(batch);
    }

    /// Get all pending batches
    pub fn get_batches(&self) -> Vec<RenderBatch> {
        self.pending_batches.lock().unwrap().clone()
    }

    /// Clear pending batches
    pub fn clear(&self) {
        self.pending_batches.lock().unwrap().clear();
    }

    /// Get state change statistics
    pub fn get_state_changes(&self) -> u64 {
        *self.state_changes.lock().unwrap()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *self.state_changes.lock().unwrap() = 0;
    }
}

impl Default for CommandBufferOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

/// GPU memory manager for efficient texture and buffer uploads
pub struct GpuMemoryManager {
    /// Maximum GPU memory (bytes)
    max_memory: u64,

    /// Currently allocated memory
    current_memory: Arc<Mutex<u64>>,
}

impl GpuMemoryManager {
    pub fn new(max_memory: u64) -> Self {
        Self {
            max_memory,
            current_memory: Arc::new(Mutex::new(0)),
        }
    }

    /// Allocate GPU memory
    pub fn allocate(&self, size: u64) -> Result<u64, String> {
        let mut current = self.current_memory.lock().unwrap();

        if *current + size > self.max_memory {
            return Err(format!(
                "GPU memory limit exceeded: {} + {} > {}",
                *current, size, self.max_memory
            ));
        }

        let offset = *current;
        *current += size;
        Ok(offset)
    }

    /// Free GPU memory
    pub fn free(&self, size: u64) {
        let mut current = self.current_memory.lock().unwrap();
        *current = current.saturating_sub(size);
    }

    /// Get current memory usage
    pub fn current_usage(&self) -> u64 {
        *self.current_memory.lock().unwrap()
    }

    /// Get available memory
    pub fn available_memory(&self) -> u64 {
        self.max_memory - self.current_usage()
    }
}

impl Default for GpuMemoryManager {
    fn default() -> Self {
        Self::new(1024 * 1024 * 512) // 512 MB default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_texture_atlas() {
        let atlas = TextureAtlas::new(512);
        let texture_data = vec![255u8; 64 * 64 * 4];

        let result = atlas.add_region(0, &texture_data, 64, 64);
        assert!(result.is_some());

        let region = atlas.get_region(0);
        assert!(region.is_some());
    }

    #[test]
    fn test_shader_cache() {
        let cache = ShaderCache::new();
        let shader_source = "
            float4 main() { return float4(1.0); }
        ";

        let result1 = cache.get_or_compile(shader_source, "test");
        let result2 = cache.get_or_compile(shader_source, "test");

        assert!(result1.is_ok());
        assert!(result2.is_ok());
    }

    #[test]
    fn test_gpu_memory_manager() {
        let mgr = GpuMemoryManager::new(1000);

        assert!(mgr.allocate(500).is_ok());
        assert_eq!(mgr.current_usage(), 500);
        assert_eq!(mgr.available_memory(), 500);

        mgr.free(200);
        assert_eq!(mgr.current_usage(), 300);
    }
}
