//! Client-side cache for expensive listing operations.
//!
//! Caches responses from `xv ls`, `xv vault list`, and `xv file list`
//! as flat JSON files organized by vault. Supports configurable TTL,
//! background refresh, and eager invalidation on writes.

pub mod manager;
pub mod models;
pub mod refresh;

pub use manager::CacheManager;
pub use models::{CacheEntry, CacheEntryType, CacheKey, CacheStatus};
