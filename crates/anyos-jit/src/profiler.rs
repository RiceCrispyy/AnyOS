//! Performance profiling infrastructure for JIT optimization
//!
//! Tracks:
//! - Function call counts
//! - Instruction execution times
//! - Memory access patterns
//! - Cache efficiency

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Performance profile data for compiled code
#[derive(Debug, Clone, Default)]
pub struct ProfileData {
    /// Total execution time in nanoseconds
    pub total_time: u64,

    /// Number of times executed
    pub execution_count: u64,

    /// Average time per execution
    pub avg_time: u64,

    /// Cache misses (estimated)
    pub cache_misses: u64,

    /// Memory allocation count
    pub memory_allocs: u32,
}

/// Per-function profiling data
struct FunctionProfile {
    call_count: Arc<AtomicU64>,
    total_time: Arc<AtomicU64>,
    last_entry: Option<Instant>,
    cache_misses: Arc<AtomicU64>,
}

/// Global profiler for function call tracking
pub struct Profiler {
    /// Function call counts: address -> count
    call_counts: DashMap<u64, Arc<AtomicU64>>,

    /// Function execution times: address -> nanoseconds
    execution_times: DashMap<u64, Arc<AtomicU64>>,

    /// Total instructions executed
    total_instructions: Arc<AtomicU64>,

    /// Total compilation time
    total_compile_time: Arc<AtomicU64>,
}

impl Profiler {
    pub fn new() -> Self {
        Self {
            call_counts: DashMap::new(),
            execution_times: DashMap::new(),
            total_instructions: Arc::new(AtomicU64::new(0)),
            total_compile_time: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Record a function call
    pub fn record_call(&self, addr: u64) {
        self.call_counts
            .entry(addr)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Get the call count for an address
    pub fn get_call_count(&self, addr: u64) -> u64 {
        self.call_counts
            .get(&addr)
            .map(|entry| entry.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Record execution time
    pub fn record_time(&self, addr: u64, nanos: u64) {
        self.execution_times
            .entry(addr)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_add(nanos, Ordering::Relaxed);
    }

    /// Get average execution time for an address
    pub fn get_avg_time(&self, addr: u64) -> u64 {
        let call_count = self.get_call_count(addr);
        if call_count == 0 {
            return 0;
        }

        self.execution_times
            .get(&addr)
            .map(|entry| {
                let total = entry.load(Ordering::Relaxed);
                total / call_count.max(1)
            })
            .unwrap_or(0)
    }

    /// Record instruction execution
    pub fn record_instruction(&self) {
        self.total_instructions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record compilation time
    pub fn record_compile_time(&self, nanos: u64) {
        self.total_compile_time.fetch_add(nanos, Ordering::Relaxed);
    }

    /// Get total instructions executed
    pub fn total_instructions(&self) -> u64 {
        self.total_instructions.load(Ordering::Relaxed)
    }

    /// Get statistics for all profiled functions
    pub fn get_stats(&self) -> Vec<(u64, u64, u64)> {
        // Returns (address, call_count, avg_time_ns)
        self.call_counts
            .iter()
            .map(|entry| {
                let addr = *entry.key();
                let count = entry.value().load(Ordering::Relaxed);
                let avg_time = self.get_avg_time(addr);
                (addr, count, avg_time)
            })
            .collect()
    }

    /// Reset all profiling data
    pub fn reset(&self) {
        self.call_counts.clear();
        self.execution_times.clear();
        self.total_instructions.store(0, Ordering::Relaxed);
        self.total_compile_time.store(0, Ordering::Relaxed);
    }
}

impl Default for Profiler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profiler_call_count() {
        let profiler = Profiler::new();
        profiler.record_call(0x1000);
        profiler.record_call(0x1000);
        profiler.record_call(0x2000);

        assert_eq!(profiler.get_call_count(0x1000), 2);
        assert_eq!(profiler.get_call_count(0x2000), 1);
        assert_eq!(profiler.get_call_count(0x3000), 0);
    }

    #[test]
    fn test_profiler_execution_time() {
        let profiler = Profiler::new();
        profiler.record_time(0x1000, 1000);
        profiler.record_time(0x1000, 2000);
        profiler.record_call(0x1000);
        profiler.record_call(0x1000);

        assert_eq!(profiler.get_avg_time(0x1000), 1500);
    }
}
