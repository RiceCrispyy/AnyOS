//! Vertex and index buffer management

use crate::{D3DError, D3DResult};
use std::sync::Arc;

/// Handle to a vertex buffer
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct VertexBufferHandle(pub u32);

/// Handle to an index buffer
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct IndexBufferHandle(pub u32);

/// Vertex buffer descriptor
#[derive(Debug, Clone)]
pub struct VertexBufferDesc {
    pub length: u32,
    pub stride: u32,
    pub usage: u32,
}

impl VertexBufferDesc {
    pub fn new(length: u32, stride: u32) -> Self {
        Self {
            length,
            stride,
            usage: 0,
        }
    }
}

/// Index buffer descriptor
#[derive(Debug, Clone)]
pub struct IndexBufferDesc {
    pub count: u32,
    pub usage: u32,
    pub format: IndexFormat,
}

impl IndexBufferDesc {
    pub fn new(count: u32, format: IndexFormat) -> Self {
        Self {
            count,
            usage: 0,
            format,
        }
    }
}

/// Index format (16-bit or 32-bit)
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IndexFormat {
    Index16 = 1,
    Index32 = 2,
}

/// Vertex buffer
pub struct VertexBuffer {
    pub handle: VertexBufferHandle,
    pub desc: VertexBufferDesc,
    pub data: Vec<u8>,
    pub locked: bool,
    pub platform_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl VertexBuffer {
    pub fn new(handle: VertexBufferHandle, desc: VertexBufferDesc) -> Self {
        let capacity = (desc.length * desc.stride) as usize;
        Self {
            handle,
            desc,
            data: vec![0u8; capacity],
            locked: false,
            platform_data: None,
        }
    }

    /// Lock the buffer for writing
    pub fn lock(&mut self) -> D3DResult<*mut u8> {
        if self.locked {
            return Err(D3DError::StateError("Buffer already locked".to_string()));
        }
        self.locked = true;
        Ok(self.data.as_mut_ptr())
    }

    /// Unlock the buffer
    pub fn unlock(&mut self) -> D3DResult<()> {
        if !self.locked {
            return Err(D3DError::StateError("Buffer not locked".to_string()));
        }
        self.locked = false;
        Ok(())
    }

    /// Write vertex data to the buffer
    pub fn write_data(&mut self, offset: u32, data: &[u8]) -> D3DResult<()> {
        if self.locked {
            return Err(D3DError::StateError(
                "Cannot write to locked buffer".to_string(),
            ));
        }

        let offset = offset as usize;
        let end = offset + data.len();

        if end > self.data.len() {
            return Err(D3DError::BufferCreationFailed(
                "Write exceeds buffer size".to_string(),
            ));
        }

        self.data[offset..end].copy_from_slice(data);
        Ok(())
    }

    /// Get vertex count
    pub fn get_vertex_count(&self) -> u32 {
        self.desc.length
    }

    /// Get stride
    pub fn get_stride(&self) -> u32 {
        self.desc.stride
    }

    /// Get raw data pointer
    pub fn get_data(&self) -> *const u8 {
        self.data.as_ptr()
    }
}

/// Index buffer
pub struct IndexBuffer {
    pub handle: IndexBufferHandle,
    pub desc: IndexBufferDesc,
    pub data: Vec<u8>,
    pub locked: bool,
    pub platform_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl IndexBuffer {
    pub fn new(handle: IndexBufferHandle, desc: IndexBufferDesc) -> Self {
        let stride = match desc.format {
            IndexFormat::Index16 => 2,
            IndexFormat::Index32 => 4,
        };
        let capacity = (desc.count as usize) * stride;

        Self {
            handle,
            desc,
            data: vec![0u8; capacity],
            locked: false,
            platform_data: None,
        }
    }

    /// Lock the buffer for writing
    pub fn lock(&mut self) -> D3DResult<*mut u8> {
        if self.locked {
            return Err(D3DError::StateError("Buffer already locked".to_string()));
        }
        self.locked = true;
        Ok(self.data.as_mut_ptr())
    }

    /// Unlock the buffer
    pub fn unlock(&mut self) -> D3DResult<()> {
        if !self.locked {
            return Err(D3DError::StateError("Buffer not locked".to_string()));
        }
        self.locked = false;
        Ok(())
    }

    /// Write index data to the buffer
    pub fn write_data(&mut self, offset: u32, data: &[u8]) -> D3DResult<()> {
        if self.locked {
            return Err(D3DError::StateError(
                "Cannot write to locked buffer".to_string(),
            ));
        }

        let offset = offset as usize;
        let end = offset + data.len();

        if end > self.data.len() {
            return Err(D3DError::BufferCreationFailed(
                "Write exceeds buffer size".to_string(),
            ));
        }

        self.data[offset..end].copy_from_slice(data);
        Ok(())
    }

    /// Get index count
    pub fn get_index_count(&self) -> u32 {
        self.desc.count
    }

    /// Get raw data pointer
    pub fn get_data(&self) -> *const u8 {
        self.data.as_ptr()
    }

    /// Get index size in bytes
    pub fn get_index_size(&self) -> u32 {
        match self.desc.format {
            IndexFormat::Index16 => 2,
            IndexFormat::Index32 => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertex_buffer_creation() {
        let desc = VertexBufferDesc::new(1000, 32); // 1000 vertices, 32 bytes each
        let handle = VertexBufferHandle(1);
        let vb = VertexBuffer::new(handle, desc);

        assert_eq!(vb.get_vertex_count(), 1000);
        assert_eq!(vb.get_stride(), 32);
        assert_eq!(vb.data.len(), 32000);
    }

    #[test]
    fn test_index_buffer_creation() {
        let desc = IndexBufferDesc::new(5000, IndexFormat::Index16);
        let handle = IndexBufferHandle(1);
        let ib = IndexBuffer::new(handle, desc);

        assert_eq!(ib.get_index_count(), 5000);
        assert_eq!(ib.get_index_size(), 2);
        assert_eq!(ib.data.len(), 10000);
    }

    #[test]
    fn test_vertex_buffer_lock() {
        let desc = VertexBufferDesc::new(100, 32);
        let handle = VertexBufferHandle(1);
        let mut vb = VertexBuffer::new(handle, desc);

        let ptr = vb.lock().unwrap();
        assert!(!ptr.is_null());
        assert!(vb.locked);

        vb.unlock().unwrap();
        assert!(!vb.locked);
    }

    #[test]
    fn test_index_buffer_write() {
        let desc = IndexBufferDesc::new(100, IndexFormat::Index16);
        let handle = IndexBufferHandle(1);
        let mut ib = IndexBuffer::new(handle, desc);

        let data = [0u16, 1, 2];
        let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, 6) };

        ib.write_data(0, bytes).unwrap();
        assert_eq!(ib.data[0..6], bytes[..]);
    }
}
