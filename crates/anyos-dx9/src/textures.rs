//! Texture management and creation

use crate::{D3DError, D3DResult, D3DFORMAT, D3DPOOL};
use std::sync::Arc;

/// Handle to a texture
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct TextureHandle(pub u32);

/// Texture descriptor
#[derive(Debug, Clone)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    pub usage: u32,
    pub format: D3DFORMAT,
    pub pool: D3DPOOL,
}

impl TextureDesc {
    pub fn new(width: u32, height: u32, format: D3DFORMAT) -> Self {
        Self {
            width,
            height,
            levels: 1,
            usage: 0,
            format,
            pool: D3DPOOL::D3DPOOL_MANAGED,
        }
    }

    pub fn with_levels(mut self, levels: u32) -> Self {
        self.levels = levels;
        self
    }
}

/// Surface level of a texture
pub struct TextureSurface {
    pub level: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub pitch: u32,
}

impl TextureSurface {
    pub fn new(level: u32, width: u32, height: u32, bytes_per_pixel: u32) -> Self {
        let pitch = width * bytes_per_pixel;
        let capacity = (pitch * height) as usize;

        Self {
            level,
            width,
            height,
            data: vec![0u8; capacity],
            pitch,
        }
    }

    pub fn lock(&mut self) -> D3DResult<*mut u8> {
        Ok(self.data.as_mut_ptr())
    }

    pub fn write_data(&mut self, offset: u32, data: &[u8]) -> D3DResult<()> {
        let offset = offset as usize;
        let end = offset + data.len();

        if end > self.data.len() {
            return Err(D3DError::TextureCreationFailed(
                "Write exceeds surface size".to_string(),
            ));
        }

        self.data[offset..end].copy_from_slice(data);
        Ok(())
    }
}

/// Texture object
pub struct Texture {
    pub handle: TextureHandle,
    pub desc: TextureDesc,
    pub surfaces: Vec<TextureSurface>,
    pub platform_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl Texture {
    pub fn new(handle: TextureHandle, desc: TextureDesc) -> Self {
        let bytes_per_pixel = Self::bytes_per_pixel(desc.format);
        let mut surfaces = Vec::new();

        for level in 0..desc.levels {
            let scale = 1u32 << level;
            let width = (desc.width / scale).max(1);
            let height = (desc.height / scale).max(1);
            surfaces.push(TextureSurface::new(level, width, height, bytes_per_pixel));
        }

        Self {
            handle,
            desc,
            surfaces,
            platform_data: None,
        }
    }

    /// Get the number of bytes per pixel for a format
    pub fn bytes_per_pixel(format: D3DFORMAT) -> u32 {
        match format {
            D3DFORMAT::D3DFMT_A8R8G8B8 | D3DFORMAT::D3DFMT_X8R8G8B8 => 4,
            D3DFORMAT::D3DFMT_R8G8B8 => 3,
            D3DFORMAT::D3DFMT_R5G6B5 | D3DFORMAT::D3DFMT_A1R5G5B5 => 2,
            D3DFORMAT::D3DFMT_A8 => 1,
            // Compressed formats have variable sizes
            D3DFORMAT::D3DFMT_DXT1 => 1, // 6 bits per pixel
            D3DFORMAT::D3DFMT_DXT3 | D3DFORMAT::D3DFMT_DXT5 => 1, // 8 bits per pixel
            _ => 4, // Default to 4 for unknown formats
        }
    }

    /// Get a surface by level
    pub fn get_surface(&self, level: u32) -> D3DResult<&TextureSurface> {
        self.surfaces.get(level as usize).ok_or_else(|| {
            D3DError::InvalidHandle(format!("Mipmap level {} out of range", level))
        })
    }

    /// Get a mutable surface by level
    pub fn get_surface_mut(&mut self, level: u32) -> D3DResult<&mut TextureSurface> {
        if level >= self.surfaces.len() as u32 {
            return Err(D3DError::InvalidHandle(format!(
                "Mipmap level {} out of range",
                level
            )));
        }
        Ok(&mut self.surfaces[level as usize])
    }

    /// Lock a surface level for writing
    pub fn lock_surface(&mut self, level: u32) -> D3DResult<*mut u8> {
        let surface = self.get_surface_mut(level)?;
        surface.lock()
    }

    /// Get texture size in bytes (all mipmap levels)
    pub fn get_size(&self) -> u32 {
        self.surfaces.iter().map(|s| s.data.len() as u32).sum()
    }

    /// Get raw data pointer for level 0
    pub fn get_data(&self) -> *const u8 {
        self.surfaces
            .first()
            .map(|s| s.data.as_ptr())
            .unwrap_or(std::ptr::null())
    }
}

/// Helper to convert RGB color to texture pixel
pub fn rgb_to_argb8888(r: u8, g: u8, b: u8) -> u32 {
    0xFF000000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Helper to convert RGBA color to texture pixel
pub fn rgba_to_argb8888(r: u8, g: u8, b: u8, a: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_texture_creation() {
        let desc = TextureDesc::new(512, 512, D3DFORMAT::D3DFMT_A8R8G8B8);
        let handle = TextureHandle(1);
        let tex = Texture::new(handle, desc);

        assert_eq!(tex.desc.width, 512);
        assert_eq!(tex.desc.height, 512);
        assert_eq!(tex.surfaces.len(), 1);
        assert_eq!(tex.surfaces[0].data.len(), 512 * 512 * 4);
    }

    #[test]
    fn test_texture_with_mipmaps() {
        let desc = TextureDesc::new(256, 256, D3DFORMAT::D3DFMT_A8R8G8B8)
            .with_levels(5);
        let handle = TextureHandle(1);
        let tex = Texture::new(handle, desc);

        assert_eq!(tex.surfaces.len(), 5);
        assert_eq!(tex.surfaces[0].width, 256);
        assert_eq!(tex.surfaces[1].width, 128);
        assert_eq!(tex.surfaces[2].width, 64);
        assert_eq!(tex.surfaces[3].width, 32);
        assert_eq!(tex.surfaces[4].width, 16);
    }

    #[test]
    fn test_bytes_per_pixel() {
        assert_eq!(Texture::bytes_per_pixel(D3DFORMAT::D3DFMT_A8R8G8B8), 4);
        assert_eq!(Texture::bytes_per_pixel(D3DFORMAT::D3DFMT_R8G8B8), 3);
        assert_eq!(Texture::bytes_per_pixel(D3DFORMAT::D3DFMT_R5G6B5), 2);
        assert_eq!(Texture::bytes_per_pixel(D3DFORMAT::D3DFMT_A8), 1);
    }

    #[test]
    fn test_color_conversion() {
        let color = rgba_to_argb8888(255, 128, 64, 200);
        assert_eq!(color, 0xC8FF8040);
    }
}
