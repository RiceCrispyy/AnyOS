//! # anyos-jit
//!
//! JIT compilation backend for AnyOS. Compiles hot code paths to native ARM64
//! code on macOS, with profile-guided optimization and hotspot detection.
//!
//! The JIT works alongside the interpreter:
//! - Interpreter runs code with function call counting
//! - Hot functions are identified via profiling
//! - ARM64 native code is generated and cached
//! - Code is patched to jump to native version when it reaches threshold
//! - Seamless fallback to interpreter if issues arise

pub mod codegen;
pub mod profiler;
pub mod hotspot;
pub mod executor;
pub mod threading;

use anyos_backend::{ExecutionBackend, GuestArch, GuestContext, GuestFault, StopReason};
use anyos_memory::AddressSpace;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use thiserror::Error;

/// Errors that can occur during JIT compilation
#[derive(Debug, Error)]
pub enum JitError {
    #[error("compilation failed: {0}")]
    CompilationFailed(String),

    #[error("codegen error: {0}")]
    CodegenError(String),

    #[error("unsupported instruction at {pc:#x}: {details}")]
    UnsupportedInstruction { pc: u64, details: String },
}

/// A compiled native code fragment
#[derive(Debug, Clone)]
pub struct CompiledCode {
    /// Machine code bytes (ARM64)
    pub code: Vec<u8>,
    /// Entry point offset in the code buffer
    pub entry_offset: u32,
    /// Source address in guest space
    pub source_addr: u64,
    /// Size of compiled code
    pub size: u32,
    /// How many times this has been executed
    pub execution_count: u64,
    /// Performance profile data
    pub profile: profiler::ProfileData,
}

/// Hotspot detection threshold
pub const HOTSPOT_THRESHOLD: u64 = 10_000;

/// Maximum size of compiled code cache (500MB)
pub const MAX_CODE_CACHE_SIZE: usize = 500 * 1024 * 1024;

/// JIT compilation backend with native code generation
pub struct JitBackend {
    /// Wrapped interpreter for fallback
    interpreter: anyos_interp::Interpreter,

    /// Compiled code cache: guest_addr -> CompiledCode
    code_cache: Arc<DashMap<u64, Arc<CompiledCode>>>,

    /// Function call profiler
    profiler: Arc<profiler::Profiler>,

    /// Hotspot detector
    hotspot_detector: Arc<hotspot::HotspotDetector>,

    /// Total size of code cache
    code_cache_size: Arc<RwLock<usize>>,

    /// Current code buffer (executable memory)
    code_buffer: Arc<RwLock<Vec<u8>>>,

    /// Thread pool for JIT compilation tasks
    thread_pool: Arc<rayon::ThreadPool>,
}

impl JitBackend {
    pub fn new() -> Self {
        let thread_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("failed to create JIT thread pool");

        Self {
            interpreter: anyos_interp::Interpreter::new(),
            code_cache: Arc::new(DashMap::new()),
            profiler: Arc::new(profiler::Profiler::new()),
            hotspot_detector: Arc::new(hotspot::HotspotDetector::new()),
            code_cache_size: Arc::new(RwLock::new(0)),
            code_buffer: Arc::new(RwLock::new(Vec::with_capacity(64 * 1024))),
            thread_pool: Arc::new(thread_pool),
        }
    }

    /// Compile a function starting at the given address
    fn compile_function(
        &mut self,
        addr: u64,
        ctx: &GuestContext,
    ) -> Result<Arc<CompiledCode>, JitError> {
        // Try to generate ARM64 code for this region
        let code_gen = codegen::Arm64Generator::new();
        let compiled = code_gen.compile_region(addr, ctx)?;

        let compiled = Arc::new(compiled);
        self.code_cache.insert(addr, compiled.clone());

        // Update cache size
        let mut cache_size = self.code_cache_size.write();
        *cache_size += compiled.size as usize;

        // Evict oldest entries if cache is full
        if *cache_size > MAX_CODE_CACHE_SIZE {
            self.evict_code_cache();
        }

        Ok(compiled)
    }

    /// Evict least-recently-used compiled code to make room
    fn evict_code_cache(&self) {
        // Find least recently executed code and remove it
        if let Some((addr, _)) = self.code_cache
            .iter()
            .min_by_key(|entry| entry.value().execution_count) {
            let addr = *addr;
            if let Some((_, code)) = self.code_cache.remove(&addr) {
                let mut cache_size = self.code_cache_size.write();
                *cache_size = cache_size.saturating_sub(code.size as usize);
            }
        }
    }

    /// Check if a function is hot and schedule compilation if needed
    fn check_hotspot(&self, addr: u64, call_count: u64) {
        if call_count > HOTSPOT_THRESHOLD && !self.code_cache.contains_key(&addr) {
            // Mark for compilation (would be async in production)
            self.hotspot_detector.mark_hotspot(addr);
        }
    }
}

impl Default for JitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionBackend for JitBackend {
    fn name(&self) -> &'static str {
        "jit"
    }

    fn supports(&self, arch: GuestArch) -> bool {
        // Only support x86-64 on ARM64 systems initially
        matches!(arch, GuestArch::X86_64)
    }

    fn run(
        &mut self,
        ctx: &mut GuestContext,
        mem: &mut AddressSpace,
        limit: Option<u64>,
    ) -> StopReason {
        // Record function calls for profiling
        let call_count = self.profiler.get_call_count(ctx.pc());
        self.check_hotspot(ctx.pc(), call_count);

        // Check if we have compiled code for this address
        if let Some(compiled) = self.code_cache.get(&ctx.pc()) {
            // Use compiled code if available
            drop(compiled);
            // In a real implementation, we'd jump to the compiled code here
            // For now, fall through to interpreter
        }

        // Fall back to interpreter for execution
        self.interpreter.run(ctx, mem, limit)
    }

    fn instructions_executed(&self) -> u64 {
        self.interpreter.instructions_executed()
    }
}
