//! Common types and structures for D3D9

use glam::{Mat4, Vec3, Vec4};

/// 4x4 transformation matrix
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D3DMATRIX {
    pub m: [[f32; 4]; 4],
}

impl D3DMATRIX {
    pub fn identity() -> Self {
        Self {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn from_glam(mat: Mat4) -> Self {
        Self {
            m: mat.to_cols_array_2d(),
        }
    }

    pub fn to_glam(self) -> Mat4 {
        Mat4::from_cols_array_2d(&self.m)
    }
}

/// 3D vector
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D3DVECTOR {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl D3DVECTOR {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn zero() -> Self {
        Self { x: 0.0, y: 0.0, z: 0.0 }
    }

    pub fn from_glam(v: Vec3) -> Self {
        Self { x: v.x, y: v.y, z: v.z }
    }

    pub fn to_glam(self) -> Vec3 {
        Vec3::new(self.x, self.y, self.z)
    }
}

/// Colored vertex structure
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D3DVERTEX {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub nx: f32, // normal x
    pub ny: f32, // normal y
    pub nz: f32, // normal z
    pub u: f32,  // texture u
    pub v: f32,  // texture v
}

impl D3DVERTEX {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self {
            x,
            y,
            z,
            nx: 0.0,
            ny: 0.0,
            nz: 1.0,
            u: 0.0,
            v: 0.0,
        }
    }

    pub fn with_normal(mut self, nx: f32, ny: f32, nz: f32) -> Self {
        self.nx = nx;
        self.ny = ny;
        self.nz = nz;
        self
    }

    pub fn with_texcoord(mut self, u: f32, v: f32) -> Self {
        self.u = u;
        self.v = v;
        self
    }
}

/// Vertex with diffuse color
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D3DVERTEX_COLORED {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub diffuse: u32, // D3DCOLOR format: ARGB
    pub specular: u32,
    pub u: f32,
    pub v: f32,
}

impl D3DVERTEX_COLORED {
    pub fn new(x: f32, y: f32, z: f32, color: u32) -> Self {
        Self {
            x,
            y,
            z,
            diffuse: color,
            specular: 0xFFFFFFFF,
            u: 0.0,
            v: 0.0,
        }
    }

    pub fn with_texcoord(mut self, u: f32, v: f32) -> Self {
        self.u = u;
        self.v = v;
        self
    }
}

/// Screen space transformed vertex (used after transform and lighting)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D3DTLVERTEX {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rhw: f32, // Reciprocal homogeneous w
    pub diffuse: u32,
    pub specular: u32,
    pub u: f32,
    pub v: f32,
}

impl D3DTLVERTEX {
    pub fn new(x: f32, y: f32, z: f32, color: u32) -> Self {
        Self {
            x,
            y,
            z,
            rhw: 1.0,
            diffuse: color,
            specular: 0xFFFFFFFF,
            u: 0.0,
            v: 0.0,
        }
    }

    pub fn with_texcoord(mut self, u: f32, v: f32) -> Self {
        self.u = u;
        self.v = v;
        self
    }
}

/// D3D color as u32 (ARGB format)
pub fn make_d3dcolor(a: u8, r: u8, g: u8, b: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

pub fn make_d3dcolor_rgb(r: u8, g: u8, b: u8) -> u32 {
    make_d3dcolor(255, r, g, b)
}

/// Sampler filter types
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DTEXTUREFILTERTYPE {
    D3DTEXF_NONE = 0,
    D3DTEXF_POINT = 1,
    D3DTEXF_LINEAR = 2,
    D3DTEXF_ANISOTROPIC = 3,
    D3DTEXF_PYRAMIDALQUAD = 6,
    D3DTEXF_GAUSSIANQUAD = 7,
}

/// Address modes for texture coordinates
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DTEXTUREADDRESS {
    D3DTADDRESS_WRAP = 1,
    D3DTADDRESS_MIRROR = 2,
    D3DTADDRESS_CLAMP = 3,
    D3DTADDRESS_BORDER = 4,
    D3DTADDRESS_MIRRORONCE = 5,
}

/// Usage flags for buffers
pub const D3DUSAGE_DYNAMIC: u32 = 0x00000200;
pub const D3DUSAGE_WRITEONLY: u32 = 0x00000008;
pub const D3DUSAGE_SOFTWAREPROCESSING: u32 = 0x00000010;

/// Pool types for resource allocation
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DPOOL {
    D3DPOOL_DEFAULT = 0,
    D3DPOOL_MANAGED = 1,
    D3DPOOL_SYSTEMMEM = 2,
    D3DPOOL_SCRATCH = 3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_creation() {
        let color = make_d3dcolor_rgb(255, 128, 64);
        assert_eq!(color, 0xFFFF8040);
    }

    #[test]
    fn test_matrix_identity() {
        let mat = D3DMATRIX::identity();
        assert_eq!(mat.m[0][0], 1.0);
        assert_eq!(mat.m[1][1], 1.0);
        assert_eq!(mat.m[0][1], 0.0);
    }

    #[test]
    fn test_vertex_creation() {
        let v = D3DVERTEX::new(1.0, 2.0, 3.0);
        assert_eq!(v.x, 1.0);
        assert_eq!(v.z, 3.0);
        assert_eq!(v.nz, 1.0);
    }
}
