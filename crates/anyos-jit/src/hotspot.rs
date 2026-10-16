//! Hotspot detection and code patching
//!
//! Identifies frequently-called functions and marks them for JIT compilation.
//! Uses profile-guided optimization to improve compilation efficiency.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A detected hotspot (frequently-executed code region)
#[derive(Debug, Clone)]
pub struct Hotspot {
    /// Guest address of the hotspot
    pub addr: u64,

    /// Number of times visited
    pub visit_count: u64,

    /// Is this hotspot already being compiled?
    pub compiling: Arc<AtomicBool>,

    /// Is compilation complete?
    pub compiled: Arc<AtomicBool>,
}

/// Detects and tracks hotspots in executed code
pub struct HotspotDetector {
    /// Known hotspots: address -> Hotspot
    hotspots: Arc<Mutex<HashMap<u64, Hotspot>>>,

    /// Hotspot threshold (calls before marking as hot)
    threshold: u64,

    /// Total hotspots detected
    total_detected: Arc<AtomicU64>,
}

impl HotspotDetector {
    pub fn new() -> Self {
        Self {
            hotspots: Arc::new(Mutex::new(HashMap::new())),
            threshold: crate::HOTSPOT_THRESHOLD,
            total_detected: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Mark an address as a potential hotspot
    pub fn mark_hotspot(&self, addr: u64) {
        let mut hotspots = self.hotspots.lock().unwrap();
        if let std::collections::hash_map::Entry::Vacant(entry) = hotspots.entry(addr) {
            entry.insert(Hotspot {
                addr,
                visit_count: 1,
                compiling: Arc::new(AtomicBool::new(false)),
                compiled: Arc::new(AtomicBool::new(false)),
            });
            self.total_detected.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Increment visit count for a hotspot
    pub fn visit_hotspot(&self, addr: u64) {
        if let Some(hotspot) = self.hotspots.lock().unwrap().get_mut(&addr) {
            hotspot.visit_count += 1;
        }
    }

    /// Get all detected hotspots
    pub fn get_hotspots(&self) -> Vec<Hotspot> {
        self.hotspots.lock().unwrap().values().cloned().collect()
    }

    /// Get hotspots ready for compilation (high visit count, not yet compiled)
    pub fn get_ready_for_compilation(&self) -> Vec<Hotspot> {
        self.hotspots
            .lock()
            .unwrap()
            .values()
            .filter(|hotspot| {
                hotspot.visit_count > self.threshold
                    && !hotspot.compiled.load(Ordering::Relaxed)
                    && !hotspot.compiling.load(Ordering::Relaxed)
            })
            .cloned()
            .collect()
    }

    /// Mark a hotspot as being compiled
    pub fn mark_compiling(&self, addr: u64) {
        if let Some(hotspot) = self.hotspots.lock().unwrap().get(&addr) {
            hotspot.compiling.store(true, Ordering::Relaxed);
        }
    }

    /// Mark a hotspot as compiled
    pub fn mark_compiled(&self, addr: u64) {
        if let Some(hotspot) = self.hotspots.lock().unwrap().get(&addr) {
            hotspot.compiled.store(true, Ordering::Relaxed);
            hotspot.compiling.store(false, Ordering::Relaxed);
        }
    }

    /// Get the number of detected hotspots
    pub fn count(&self) -> usize {
        self.hotspots.lock().unwrap().len()
    }

    /// Clear all hotspot data
    pub fn reset(&self) {
        self.hotspots.lock().unwrap().clear();
        self.total_detected.store(0, Ordering::Relaxed);
    }
}

impl Default for HotspotDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotspot_detection() {
        let detector = HotspotDetector::new();
        detector.mark_hotspot(0x1000);
        detector.mark_hotspot(0x2000);
        detector.mark_hotspot(0x1000); // Duplicate, should not create new entry

        assert_eq!(detector.count(), 2);
    }

    #[test]
    fn test_ready_for_compilation() {
        let detector = HotspotDetector {
            hotspots: Arc::new(Mutex::new(HashMap::new())),
            threshold: 5,
            total_detected: Arc::new(AtomicU64::new(0)),
        };

        let hotspot = Hotspot {
            addr: 0x1000,
            visit_count: 10,
            compiling: Arc::new(AtomicBool::new(false)),
            compiled: Arc::new(AtomicBool::new(false)),
        };

        detector.hotspots.lock().unwrap().insert(0x1000, hotspot);

        let ready = detector.get_ready_for_compilation();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].addr, 0x1000);
    }
}
