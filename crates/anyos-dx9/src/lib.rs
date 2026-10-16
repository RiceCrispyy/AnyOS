//! DirectX 9 API implementation for AnyOS
//!
//! Provides a subset of Direct3D 9 API that games depend on.
//! Backends (Metal on macOS, etc.) implement actual rendering.

// The Direct3D SDK spells its identifiers in SCREAMING_SNAKE_CASE
// (D3DRS_ZENABLE, D3DTS_WORLD, …). Keep them verbatim so this API mirrors the
// real thing instead of rewriting them to Rust's CamelCase.
#![allow(non_camel_case_types)]

pub mod buffers;
pub mod device;
pub mod shaders;
pub mod textures;
pub mod types;

pub use buffers::{IndexBuffer, VertexBuffer};
pub use device::{D3DDevice, D3DDeviceHandle};
pub use shaders::{PixelShader, ShaderHandle, VertexShader};
pub use textures::{Texture, TextureHandle};
pub use types::*;

use thiserror::Error;

/// Errors from the DirectX 9 implementation
#[derive(Error, Debug)]
pub enum D3DError {
    #[error("Device creation failed: {0}")]
    DeviceCreationFailed(String),

    #[error("Buffer creation failed: {0}")]
    BufferCreationFailed(String),

    #[error("Texture creation failed: {0}")]
    TextureCreationFailed(String),

    #[error("Shader compilation failed: {0}")]
    ShaderCompilationFailed(String),

    #[error("Render error: {0}")]
    RenderError(String),

    #[error("Invalid handle: {0}")]
    InvalidHandle(String),

    #[error("State error: {0}")]
    StateError(String),

    #[error("Platform error: {0}")]
    PlatformError(String),

    #[error("Out of memory")]
    OutOfMemory,
}

pub type D3DResult<T> = Result<T, D3DError>;

/// D3D Format enumerations
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DFORMAT {
    D3DFMT_UNKNOWN = 0,
    D3DFMT_R8G8B8 = 20,
    D3DFMT_A8R8G8B8 = 21,
    D3DFMT_X8R8G8B8 = 22,
    D3DFMT_R5G6B5 = 23,
    D3DFMT_X1R5G5B5 = 24,
    D3DFMT_A1R5G5B5 = 25,
    D3DFMT_A4R4G4B4 = 26,
    D3DFMT_R3G3B2 = 27,
    D3DFMT_A8 = 28,
    D3DFMT_A8R3G3B2 = 29,
    D3DFMT_X4R4G4B4 = 30,
    D3DFMT_DXT1 = 0x31545844, // 'DXT1'
    D3DFMT_DXT2 = 0x32545844, // 'DXT2'
    D3DFMT_DXT3 = 0x33545844, // 'DXT3'
    D3DFMT_DXT4 = 0x34545844, // 'DXT4'
    D3DFMT_DXT5 = 0x35545844, // 'DXT5'
}

/// Primitive types for DrawPrimitive
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DPRIMITIVETYPE {
    D3DPT_POINTLIST = 1,
    D3DPT_LINELIST = 2,
    D3DPT_LINESTRIP = 3,
    D3DPT_TRIANGLELIST = 4,
    D3DPT_TRIANGLESTRIP = 5,
    D3DPT_TRIANGLEFAN = 6,
}

/// Blend operations
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DBLEND {
    D3DBLEND_ZERO = 1,
    D3DBLEND_ONE = 2,
    D3DBLEND_SRCCOLOR = 3,
    D3DBLEND_INVSRCCOLOR = 4,
    D3DBLEND_SRCALPHA = 5,
    D3DBLEND_INVSRCALPHA = 6,
    D3DBLEND_DESTALPHA = 7,
    D3DBLEND_INVDESTALPHA = 8,
    D3DBLEND_DESTCOLOR = 9,
    D3DBLEND_INVDESTCOLOR = 10,
    D3DBLEND_SRCALPHASAT = 11,
    D3DBLEND_BOTHSRCALPHA = 12,
    D3DBLEND_BOTHINVSRCALPHA = 13,
    D3DBLEND_BLENDFACTOR = 14,
    D3DBLEND_INVBLENDFACTOR = 15,
    D3DBLEND_SRCCOLOR2 = 16,
    D3DBLEND_INVSRCCOLOR2 = 17,
}

/// Compare functions
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DCMPFUNC {
    D3DCMP_NEVER = 1,
    D3DCMP_LESS = 2,
    D3DCMP_EQUAL = 3,
    D3DCMP_LESSEQUAL = 4,
    D3DCMP_GREATER = 5,
    D3DCMP_NOTEQUAL = 6,
    D3DCMP_GREATEREQUAL = 7,
    D3DCMP_ALWAYS = 8,
}

/// Rasterizer fill modes
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DFILLMODE {
    D3DFILL_POINT = 1,
    D3DFILL_WIREFRAME = 2,
    D3DFILL_SOLID = 3,
}

/// Cull modes
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum D3DCULL {
    D3DCULL_NONE = 1,
    D3DCULL_CW = 2,
    D3DCULL_CCW = 3,
}

/// Matrix types for SetTransform
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum D3DTRANSFORMSTATETYPE {
    D3DTS_WORLD = 256,
    D3DTS_VIEW = 2,
    D3DTS_PROJECTION = 3,
    D3DTS_TEXTURE0 = 16,
    D3DTS_TEXTURE1 = 17,
    D3DTS_TEXTURE2 = 18,
    D3DTS_TEXTURE3 = 19,
    D3DTS_TEXTURE4 = 20,
    D3DTS_TEXTURE5 = 21,
    D3DTS_TEXTURE6 = 22,
    D3DTS_TEXTURE7 = 23,
}

/// Render states
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum D3DRENDERSTATETYPE {
    D3DRS_ZENABLE = 7,
    D3DRS_FILLMODE = 8,
    D3DRS_SHADEMODE = 9,
    D3DRS_ZWRITEENABLE = 14,
    D3DRS_ALPHATESTENABLE = 15,
    D3DRS_LASTPIXEL = 16,
    D3DRS_SRCBLEND = 19,
    D3DRS_DESTBLEND = 20,
    D3DRS_CULLMODE = 22,
    D3DRS_ZFUNC = 23,
    D3DRS_ALPHAREF = 24,
    D3DRS_ALPHAFUNC = 25,
    D3DRS_DITHERENABLE = 26,
    D3DRS_ALPHABLENDENABLE = 27,
    D3DRS_FOGENABLE = 28,
    D3DRS_SPECULARENABLE = 29,
    D3DRS_FOGCOLOR = 34,
    D3DRS_FOGTABLEMODE = 35,
    D3DRS_FOGSTART = 36,
    D3DRS_FOGEND = 37,
    D3DRS_FOGDENSITY = 38,
    D3DRS_RANGEFOGENABLE = 48,
    D3DRS_STENCILENABLE = 52,
    D3DRS_STENCILFAIL = 53,
    D3DRS_STENCILZFAIL = 54,
    D3DRS_STENCILPASS = 55,
    D3DRS_STENCILFUNC = 56,
    D3DRS_STENCILREF = 57,
    D3DRS_STENCILMASK = 58,
    D3DRS_STENCILWRITEMASK = 59,
    D3DRS_TEXTUREFACTOR = 60,
    D3DRS_WRAP0 = 128,
    D3DRS_WRAP1 = 129,
    D3DRS_WRAP2 = 130,
    D3DRS_WRAP3 = 131,
    D3DRS_WRAP4 = 132,
    D3DRS_WRAP5 = 133,
    D3DRS_WRAP6 = 134,
    D3DRS_WRAP7 = 135,
    D3DRS_CLIPPING = 136,
    D3DRS_LIGHTING = 137,
    D3DRS_AMBIENT = 139,
    D3DRS_FOGVERTEXMODE = 140,
    D3DRS_COLORVERTEX = 141,
    D3DRS_LOCALVIEWER = 142,
    D3DRS_NORMALIZENORMALS = 143,
    D3DRS_DIFFUSEMATERIALSOURCE = 145,
    D3DRS_SPECULARMATERIALSOURCE = 146,
    D3DRS_AMBIENTMATERIALSOURCE = 147,
    D3DRS_EMISSIVEMATERIALSOURCE = 148,
    D3DRS_VERTEXBLEND = 151,
    D3DRS_CLIPPLANEENABLE = 152,
    D3DRS_POINTSIZE = 154,
    D3DRS_POINTSIZE_MIN = 155,
    D3DRS_POINTSPRITEENABLE = 156,
    D3DRS_POINTSCALEENABLE = 157,
    D3DRS_POINTSCALE_A = 158,
    D3DRS_POINTSCALE_B = 159,
    D3DRS_POINTSCALE_C = 160,
    D3DRS_MULTISAMPLEANTIALIAS = 161,
    D3DRS_MULTISAMPLEMASK = 162,
    D3DRS_PATCHEDGESTYLE = 163,
    D3DRS_DEBUGMONITORTOKEN = 165,
    D3DRS_POINTSIZE_MAX = 166,
    D3DRS_INDEXEDVERTEXBLENDENABLE = 167,
    D3DRS_COLORWRITEENABLE = 168,
    D3DRS_TWEENFACTOR = 170,
    D3DRS_BLENDOP = 171,
    D3DRS_POSITIONORDER = 172,
    D3DRS_NORMALORDER = 173,
}

/// Sampler states
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum D3DSAMPLERSTATETYPE {
    D3DSAMP_ADDRESSU = 1,
    D3DSAMP_ADDRESSV = 2,
    D3DSAMP_ADDRESSW = 3,
    D3DSAMP_BORDERCOLOR = 4,
    D3DSAMP_MAGFILTER = 5,
    D3DSAMP_MINFILTER = 6,
    D3DSAMP_MIPFILTER = 7,
    D3DSAMP_MIPMAPLODBIAS = 8,
    D3DSAMP_MAXMIPLEVEL = 9,
    D3DSAMP_MAXANISOTROPY = 10,
    D3DSAMP_SRGBTEXTURE = 11,
    D3DSAMP_ELEMENTINDEX = 12,
    D3DSAMP_DMAPOFFSET = 13,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_values() {
        assert_eq!(D3DFORMAT::D3DFMT_A8R8G8B8 as u32, 21);
        assert_eq!(D3DFORMAT::D3DFMT_DXT1 as u32, 0x31545844);
    }

    #[test]
    fn test_primitive_types() {
        assert_eq!(D3DPRIMITIVETYPE::D3DPT_TRIANGLELIST as u32, 4);
    }
}
