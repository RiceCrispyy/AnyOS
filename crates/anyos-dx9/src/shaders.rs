//! Shader compilation and management

use crate::{D3DError, D3DResult};
use std::sync::Arc;

/// Handle to a vertex shader
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct VertexShaderHandle(pub u32);

/// Handle to a pixel/fragment shader
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct PixelShaderHandle(pub u32);

/// Generic shader handle (for texture stage states, etc)
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct ShaderHandle(pub u32);

/// Vertex shader object
pub struct VertexShader {
    pub handle: VertexShaderHandle,
    pub code: Vec<u8>,
    pub platform_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl VertexShader {
    pub fn new(handle: VertexShaderHandle, code: Vec<u8>) -> Self {
        Self {
            handle,
            code,
            platform_data: None,
        }
    }

    /// Create from HLSL source (simplified)
    pub fn from_hlsl(handle: VertexShaderHandle, hlsl: &str) -> D3DResult<Self> {
        // In a real implementation, this would compile HLSL to bytecode
        // For now, we just store the source as UTF-8 bytes
        Ok(Self::new(handle, hlsl.as_bytes().to_vec()))
    }

    pub fn get_code(&self) -> &[u8] {
        &self.code
    }
}

/// Pixel shader object
pub struct PixelShader {
    pub handle: PixelShaderHandle,
    pub code: Vec<u8>,
    pub platform_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl PixelShader {
    pub fn new(handle: PixelShaderHandle, code: Vec<u8>) -> Self {
        Self {
            handle,
            code,
            platform_data: None,
        }
    }

    /// Create from HLSL source (simplified)
    pub fn from_hlsl(handle: PixelShaderHandle, hlsl: &str) -> D3DResult<Self> {
        // In a real implementation, this would compile HLSL to bytecode
        // For now, we just store the source as UTF-8 bytes
        Ok(Self::new(handle, hlsl.as_bytes().to_vec()))
    }

    pub fn get_code(&self) -> &[u8] {
        &self.code
    }
}

/// Vertex shader constant buffer
pub struct VertexShaderConstants {
    pub data: Vec<f32>,
    pub dirty: bool,
}

impl VertexShaderConstants {
    pub fn new(size_floats: usize) -> Self {
        Self {
            data: vec![0.0f32; size_floats],
            dirty: true,
        }
    }

    pub fn set_constant(&mut self, index: usize, value: f32) -> D3DResult<()> {
        if index >= self.data.len() {
            return Err(D3DError::StateError(
                "Constant index out of range".to_string(),
            ));
        }
        self.data[index] = value;
        self.dirty = true;
        Ok(())
    }

    pub fn set_vector(&mut self, index: usize, values: &[f32]) -> D3DResult<()> {
        if index + values.len() > self.data.len() {
            return Err(D3DError::StateError(
                "Vector write exceeds buffer".to_string(),
            ));
        }
        self.data[index..index + values.len()].copy_from_slice(values);
        self.dirty = true;
        Ok(())
    }

    pub fn set_matrix(&mut self, index: usize, matrix: &[[f32; 4]; 4]) -> D3DResult<()> {
        if index + 16 > self.data.len() {
            return Err(D3DError::StateError(
                "Matrix write exceeds buffer".to_string(),
            ));
        }
        for (i, row) in matrix.iter().enumerate() {
            for (j, &val) in row.iter().enumerate() {
                self.data[index + i * 4 + j] = val;
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub fn get_data(&self) -> &[f32] {
        &self.data
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }
}

/// Pixel shader constant buffer
pub struct PixelShaderConstants {
    pub data: Vec<f32>,
    pub dirty: bool,
}

impl PixelShaderConstants {
    pub fn new(size_floats: usize) -> Self {
        Self {
            data: vec![0.0f32; size_floats],
            dirty: true,
        }
    }

    pub fn set_constant(&mut self, index: usize, value: f32) -> D3DResult<()> {
        if index >= self.data.len() {
            return Err(D3DError::StateError(
                "Constant index out of range".to_string(),
            ));
        }
        self.data[index] = value;
        self.dirty = true;
        Ok(())
    }

    pub fn get_data(&self) -> &[f32] {
        &self.data
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }
}

/// Fixed function vertex shader (fallback)
pub fn create_fixed_function_vs() -> String {
    r#"
    // Simple fixed-function vertex shader
    float4x4 matWorld : register(c0);
    float4x4 matView : register(c4);
    float4x4 matProj : register(c8);

    struct VS_INPUT {
        float3 position : POSITION;
        float3 normal : NORMAL;
        float2 texCoord : TEXCOORD0;
        float4 color : COLOR;
    };

    struct VS_OUTPUT {
        float4 position : POSITION;
        float4 color : COLOR;
        float2 texCoord : TEXCOORD0;
    };

    VS_OUTPUT main(VS_INPUT input) {
        VS_OUTPUT output;

        float4 worldPos = mul(float4(input.position, 1.0), matWorld);
        float4 viewPos = mul(worldPos, matView);
        output.position = mul(viewPos, matProj);

        output.color = input.color;
        output.texCoord = input.texCoord;

        return output;
    }
    "#
    .to_string()
}

/// Fixed function pixel shader (fallback)
pub fn create_fixed_function_ps() -> String {
    r#"
    sampler2D texture0 : register(s0);

    struct PS_INPUT {
        float4 color : COLOR;
        float2 texCoord : TEXCOORD0;
    };

    struct PS_OUTPUT {
        float4 color : COLOR;
    };

    PS_OUTPUT main(PS_INPUT input) {
        PS_OUTPUT output;
        output.color = tex2D(texture0, input.texCoord) * input.color;
        return output;
    }
    "#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertex_shader_creation() {
        let code = b"test code".to_vec();
        let handle = VertexShaderHandle(1);
        let vs = VertexShader::new(handle, code.clone());

        assert_eq!(vs.handle, handle);
        assert_eq!(vs.get_code(), b"test code");
    }

    #[test]
    fn test_vs_constants() {
        let mut consts = VertexShaderConstants::new(16);
        assert!(consts.dirty);

        consts.set_constant(0, 1.5).unwrap();
        assert_eq!(consts.data[0], 1.5);
        assert!(consts.dirty);

        consts.clear_dirty();
        assert!(!consts.dirty);
    }

    #[test]
    fn test_vs_matrix() {
        let mut consts = VertexShaderConstants::new(16);
        let matrix = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];

        consts.set_matrix(0, &matrix).unwrap();
        assert_eq!(consts.data[0], 1.0);
        assert_eq!(consts.data[5], 1.0);
    }
}
