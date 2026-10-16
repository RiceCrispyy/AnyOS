//! Performance optimization tests and benchmarks

#[cfg(test)]
mod tests {
    use anyos_jit::executor::{FrameRateMonitor, MemoryAllocator};
    use anyos_jit::hotspot::HotspotDetector;
    use anyos_jit::profiler::Profiler;
    use std::time::Instant;

    #[test]
    fn test_profiler_performance() {
        let profiler = Profiler::new();

        // Simulate function calls
        for _ in 0..100_000 {
            profiler.record_call(0x1000);
            profiler.record_call(0x2000);
            profiler.record_call(0x3000);
        }

        assert_eq!(profiler.get_call_count(0x1000), 100_000);
        assert_eq!(profiler.get_call_count(0x2000), 100_000);
        assert_eq!(profiler.get_call_count(0x3000), 100_000);
    }

    #[test]
    fn test_hotspot_detection_throughput() {
        let detector = HotspotDetector::new();
        let start = Instant::now();

        // Detect 10,000 hotspots
        for i in 0..10_000 {
            detector.mark_hotspot(0x1000 + (i as u64 * 4));
        }

        let elapsed = start.elapsed();
        println!(
            "Hotspot detection: {} hotspots in {:?}",
            detector.count(),
            elapsed
        );

        assert_eq!(detector.count(), 10_000);
        assert!(elapsed.as_millis() < 100, "Hotspot detection too slow");
    }

    #[test]
    fn test_frame_rate_monitor_60fps() {
        let monitor = FrameRateMonitor::new(60);

        // Simulate 60 frames at 16.67ms each
        for _ in 0..60 {
            let timing = anyos_jit::executor::FrameTiming {
                start: Instant::now(),
                end: None,
                cpu_time: 16_666_667, // 16.67ms in nanoseconds
                gpu_time: 0,
            };
            monitor.record_frame(timing);
        }

        let stats = monitor.get_stats();
        assert!(stats.frame_count > 0);
        println!(
            "Frame stats: avg={:?}ns, min={:?}ns, max={:?}ns, p95={:?}ns",
            stats.avg, stats.min, stats.max, stats.p95
        );
    }

    #[test]
    fn test_memory_allocator_stress() {
        let alloc = MemoryAllocator::new();

        // Simulate memory allocation patterns
        let mut sizes = Vec::new();
        for i in 0..1000 {
            let size = ((i * 13) % 10_000) as u64 + 100;
            alloc.record_alloc(size);
            sizes.push(size);
        }

        // Deallocate half
        for size in sizes.iter().take(500) {
            alloc.record_dealloc(*size);
        }

        let stats = alloc.get_stats();
        println!(
            "Memory stats: peak={}MB, current={}MB",
            stats.peak_usage / (1024 * 1024),
            stats.current_usage / (1024 * 1024)
        );

        assert!(stats.peak_usage > 0);
    }

    #[test]
    fn test_thread_pool_throughput() {
        use anyos_jit::threading::ThreadPool;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        let pool = ThreadPool::new(4);
        let counter = Arc::new(AtomicU64::new(0));

        let start = Instant::now();

        // Execute 10,000 tasks
        for _ in 0..10_000 {
            let cnt = counter.clone();
            pool.execute(move || {
                cnt.fetch_add(1, Ordering::Relaxed);
            });
        }

        pool.wait_idle();
        let elapsed = start.elapsed();

        println!("Thread pool: 10000 tasks in {:?}", elapsed);
        assert_eq!(counter.load(Ordering::Relaxed), 10_000);
        assert!(elapsed.as_millis() < 1000, "Thread pool too slow");
    }

    #[test]
    fn test_concurrent_profiling() {
        use std::thread;

        let profiler = std::sync::Arc::new(Profiler::new());
        let mut handles = Vec::new();

        // Spawn 4 threads doing profiling
        for t in 0..4 {
            let prof = profiler.clone();
            let handle = thread::spawn(move || {
                for i in 0..5000 {
                    let addr = 0x1000 + ((t * 5000 + i) as u64 * 4);
                    prof.record_call(addr);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let stats = profiler.get_stats();
        println!("Concurrent profiling: {} unique addresses", stats.len());
        assert!(!stats.is_empty());
    }
}
