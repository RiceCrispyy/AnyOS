//! Execution environment for JIT-compiled and interpreted code
//!
//! Manages:
//! - Thread pool for compilation tasks
//! - Memory management for compiled code
//! - Execution state tracking
//! - Performance monitoring

use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

/// Frame timing information for performance monitoring
#[derive(Debug, Clone, Copy)]
pub struct FrameTiming {
    /// Time the frame started
    pub start: Instant,

    /// Time the frame ended
    pub end: Option<Instant>,

    /// CPU time (nanoseconds)
    pub cpu_time: u64,

    /// GPU time (nanoseconds)
    pub gpu_time: u64,
}

impl FrameTiming {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            end: None,
            cpu_time: 0,
            gpu_time: 0,
        }
    }

    pub fn finish(&mut self) {
        self.end = Some(Instant::now());
        self.cpu_time = self.end.unwrap().duration_since(self.start).as_nanos() as u64;
    }

    pub fn total_time(&self) -> u64 {
        self.cpu_time + self.gpu_time
    }
}

impl Default for FrameTiming {
    fn default() -> Self {
        Self::new()
    }
}

/// Frame rate monitor for tracking performance
pub struct FrameRateMonitor {
    /// Frame timings (rolling buffer of last 60 frames)
    frames: Arc<RwLock<Vec<FrameTiming>>>,

    /// Target frame time (nanoseconds) - 60 FPS = ~16.67ms
    target_frame_time: u64,

    /// Maximum frames to track
    max_frames: usize,
}

impl FrameRateMonitor {
    pub fn new(target_fps: u32) -> Self {
        let target_frame_time = 1_000_000_000u64 / target_fps as u64;

        Self {
            frames: Arc::new(RwLock::new(Vec::with_capacity(60))),
            target_frame_time,
            max_frames: 60,
        }
    }

    /// Record a completed frame
    pub fn record_frame(&self, mut timing: FrameTiming) {
        timing.finish();

        let mut frames = self.frames.write();
        frames.push(timing);

        // Keep only the last N frames
        if frames.len() > self.max_frames {
            frames.remove(0);
        }
    }

    /// Get the current frame rate (average of last frames)
    pub fn current_fps(&self) -> f64 {
        let frames = self.frames.read();
        if frames.is_empty() {
            return 0.0;
        }

        let total_time: u64 = frames.iter().map(|f| f.total_time()).sum();
        let avg_frame_time = total_time / frames.len() as u64;

        if avg_frame_time == 0 {
            return 0.0;
        }

        1_000_000_000.0 / avg_frame_time as f64
    }

    /// Get frame time statistics
    pub fn get_stats(&self) -> FrameStats {
        let frames = self.frames.read();

        if frames.is_empty() {
            return FrameStats::default();
        }

        let mut times: Vec<u64> = frames.iter().map(|f| f.total_time()).collect();
        times.sort_unstable();

        let min = times[0];
        let max = times[times.len() - 1];
        let avg = times.iter().sum::<u64>() / times.len() as u64;
        let p95 = times[(times.len() * 95) / 100];
        let p99 = times[(times.len() * 99) / 100];

        FrameStats {
            min,
            max,
            avg,
            p95,
            p99,
            frame_count: frames.len(),
        }
    }

    /// Check if we're meeting frame time targets
    pub fn is_meeting_target(&self) -> bool {
        let stats = self.get_stats();
        stats.avg <= self.target_frame_time
    }
}

impl Default for FrameRateMonitor {
    fn default() -> Self {
        Self::new(60)
    }
}

/// Frame time statistics
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub min: u64,
    pub max: u64,
    pub avg: u64,
    pub p95: u64,
    pub p99: u64,
    pub frame_count: usize,
}

/// Memory allocator tracking for heap optimization
pub struct MemoryAllocator {
    /// Total allocations
    total_allocs: Arc<RwLock<u64>>,

    /// Total deallocations
    total_deallocs: Arc<RwLock<u64>>,

    /// Current memory in use (bytes)
    current_usage: Arc<RwLock<u64>>,

    /// Peak memory usage (bytes)
    peak_usage: Arc<RwLock<u64>>,
}

impl MemoryAllocator {
    pub fn new() -> Self {
        Self {
            total_allocs: Arc::new(RwLock::new(0)),
            total_deallocs: Arc::new(RwLock::new(0)),
            current_usage: Arc::new(RwLock::new(0)),
            peak_usage: Arc::new(RwLock::new(0)),
        }
    }

    /// Record an allocation
    pub fn record_alloc(&self, size: u64) {
        *self.total_allocs.write() += 1;

        let mut usage = self.current_usage.write();
        *usage += size;

        let mut peak = self.peak_usage.write();
        if *usage > *peak {
            *peak = *usage;
        }
    }

    /// Record a deallocation
    pub fn record_dealloc(&self, size: u64) {
        *self.total_deallocs.write() += 1;

        let mut usage = self.current_usage.write();
        *usage = usage.saturating_sub(size);
    }

    /// Get memory statistics
    pub fn get_stats(&self) -> MemoryStats {
        MemoryStats {
            total_allocs: *self.total_allocs.read(),
            total_deallocs: *self.total_deallocs.read(),
            current_usage: *self.current_usage.read(),
            peak_usage: *self.peak_usage.read(),
        }
    }
}

impl Default for MemoryAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Memory statistics
#[derive(Debug, Clone, Copy)]
pub struct MemoryStats {
    pub total_allocs: u64,
    pub total_deallocs: u64,
    pub current_usage: u64,
    pub peak_usage: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_frame_rate_monitor() {
        let monitor = FrameRateMonitor::new(60);

        for _ in 0..5 {
            let timing = FrameTiming {
                start: Instant::now(),
                end: None,
                cpu_time: 16_666_666, // 16.67ms in nanoseconds
                gpu_time: 0,
            };
            monitor.record_frame(timing);
            thread::sleep(Duration::from_millis(1));
        }

        let stats = monitor.get_stats();
        assert!(stats.frame_count > 0);
    }

    #[test]
    fn test_memory_allocator() {
        let alloc = MemoryAllocator::new();

        alloc.record_alloc(1000);
        alloc.record_alloc(2000);

        let stats = alloc.get_stats();
        assert_eq!(stats.total_allocs, 2);
        assert_eq!(stats.current_usage, 3000);
        assert_eq!(stats.peak_usage, 3000);

        alloc.record_dealloc(1000);
        let stats = alloc.get_stats();
        assert_eq!(stats.current_usage, 2000);
    }
}
