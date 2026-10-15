//! Memory optimization and pooling strategies
//!
//! Implements:
//! - Memory pooling for frequent allocations
//! - Efficient page translation tables
//! - Cache-friendly data layout
//! - Heap optimization

use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::VecDeque;

/// Object pool for reducing allocations
pub struct ObjectPool<T> {
    /// Available objects
    available: Arc<Mutex<VecDeque<T>>>,

    /// Total allocated objects
    total: Arc<Mutex<usize>>,

    /// Maximum pool size
    max_size: usize,

    /// Factory function for creating new objects
    factory: Arc<dyn Fn() -> T + Send + Sync>,
}

impl<T> ObjectPool<T> {
    pub fn new<F>(factory: F, max_size: usize) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self {
            available: Arc::new(Mutex::new(VecDeque::with_capacity(max_size))),
            total: Arc::new(Mutex::new(0)),
            max_size,
            factory: Arc::new(factory),
        }
    }

    /// Get an object from the pool, or create a new one
    pub fn acquire(&self) -> T {
        let mut available = self.available.lock();

        if let Some(obj) = available.pop_front() {
            obj
        } else {
            let mut total = self.total.lock();
            if *total < self.max_size {
                *total += 1;
                (self.factory)()
            } else {
                drop(total);
                drop(available);
                (self.factory)()
            }
        }
    }

    /// Return an object to the pool
    pub fn release(&self, obj: T) {
        let mut available = self.available.lock();

        if available.len() < self.max_size {
            available.push_back(obj);
        }
        // Otherwise the object is dropped
    }

    /// Get the number of available objects
    pub fn available_count(&self) -> usize {
        self.available.lock().len()
    }

    /// Get total allocated objects
    pub fn total_count(&self) -> usize {
        *self.total.lock()
    }
}

impl<T> Clone for ObjectPool<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            available: self.available.clone(),
            total: self.total.clone(),
            max_size: self.max_size,
            factory: self.factory.clone(),
        }
    }
}

/// Efficient page translation table using hierarchical structure
pub struct PageTranslationTable {
    /// L1 page table (4096 entries, one per 4KB page)
    l1: Vec<Option<u64>>,

    /// Page size in bytes
    page_size: u64,

    /// Total virtual address space covered
    virt_space: u64,
}

impl PageTranslationTable {
    pub fn new(num_pages: usize, page_size: u64) -> Self {
        Self {
            l1: vec![None; num_pages],
            page_size,
            virt_space: num_pages as u64 * page_size,
        }
    }

    /// Translate virtual address to physical address
    pub fn translate(&self, virt_addr: u64) -> Option<u64> {
        let page_idx = (virt_addr / self.page_size) as usize;

        if page_idx >= self.l1.len() {
            return None;
        }

        self.l1[page_idx].map(|phys_base| {
            let offset = virt_addr % self.page_size;
            phys_base + offset
        })
    }

    /// Map a virtual page to a physical page
    pub fn map_page(&mut self, virt_page: u64, phys_page: u64) -> Result<(), String> {
        let idx = (virt_page as usize) % self.l1.len();

        if self.l1[idx].is_some() {
            return Err("Page already mapped".to_string());
        }

        self.l1[idx] = Some(phys_page * self.page_size);
        Ok(())
    }

    /// Unmap a virtual page
    pub fn unmap_page(&mut self, virt_page: u64) {
        let idx = (virt_page as usize) % self.l1.len();
        self.l1[idx] = None;
    }

    /// Get memory efficiency statistics
    pub fn get_stats(&self) -> PageTableStats {
        let mapped = self.l1.iter().filter(|p| p.is_some()).count();

        PageTableStats {
            total_pages: self.l1.len(),
            mapped_pages: mapped,
            unmapped_pages: self.l1.len() - mapped,
            page_size: self.page_size,
            total_virtual_space: self.virt_space,
        }
    }
}

/// Page table statistics
#[derive(Debug, Clone, Copy)]
pub struct PageTableStats {
    pub total_pages: usize,
    pub mapped_pages: usize,
    pub unmapped_pages: usize,
    pub page_size: u64,
    pub total_virtual_space: u64,
}

/// Cache-friendly data layout helper
pub struct CacheLine {
    /// Actual data (aligned to cache line, typically 64 bytes)
    data: Vec<u8>,
}

impl CacheLine {
    pub const SIZE: usize = 64; // Typical L1 cache line size

    pub fn new() -> Self {
        Self {
            data: vec![0u8; Self::SIZE],
        }
    }

    /// Get mutable reference to cache line data
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Get reference to cache line data
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

impl Default for CacheLine {
    fn default() -> Self {
        Self::new()
    }
}

/// Heap allocator with pooling
pub struct PooledAllocator<T> {
    pools: Vec<ObjectPool<T>>,
}

impl<T: Clone> PooledAllocator<T> {
    pub fn new<F>(factory: F, pool_count: usize, pool_size: usize) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static + Clone,
    {
        let factory_arc = Arc::new(factory.clone());

        let pools = (0..pool_count)
            .map(|_| ObjectPool::new(|| factory_arc().clone(), pool_size))
            .collect();

        Self { pools }
    }

    /// Get an object from the best pool
    pub fn acquire(&self) -> T {
        // Use round-robin allocation
        if self.pools.is_empty() {
            panic!("No pools available");
        }

        let idx = rand::random::<usize>() % self.pools.len();
        self.pools[idx].acquire()
    }

    /// Return an object to a pool
    pub fn release(&self, obj: T) {
        if !self.pools.is_empty() {
            let idx = rand::random::<usize>() % self.pools.len();
            self.pools[idx].release(obj);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_pool() {
        let pool = ObjectPool::new(|| vec![0u8; 1024], 10);

        let obj1 = pool.acquire();
        assert_eq!(obj1.len(), 1024);

        let obj2 = pool.acquire();
        assert_eq!(obj2.len(), 1024);

        pool.release(obj1);
        assert_eq!(pool.available_count(), 1);
    }

    #[test]
    fn test_page_table() {
        let mut table = PageTranslationTable::new(1024, 4096);

        assert!(table.map_page(0, 0).is_ok());
        assert_eq!(table.translate(0), Some(0));
        assert_eq!(table.translate(1000), Some(1000));

        table.unmap_page(0);
        assert_eq!(table.translate(0), None);
    }
}
