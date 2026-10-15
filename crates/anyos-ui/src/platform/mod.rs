//! Platform-specific implementations

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub mod metal;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "windows")]
pub mod windows;

// Stub implementations for platforms without real backends yet
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub mod stub;
