//! Retry logic with exponential backoff
//!
//! This module provides configurable retry functionality with
//! exponential backoff for handling transient failures.

use crate::error::{CrosstacheError, Result};
use crate::utils::network::is_retryable_error;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct RetryOptions {
    pub max_retries: usize,
    pub initial_interval: Duration,
    pub max_interval: Duration,
    pub multiplier: f64,
}

impl Default for RetryOptions {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_interval: Duration::from_secs(1),
            max_interval: Duration::from_secs(30),
            multiplier: 2.0,
        }
    }
}

pub async fn retry_with_backoff<T, F, Fut>(mut operation: F, options: RetryOptions) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut interval = options.initial_interval;
    let mut last_error = None;

    for attempt in 0..=options.max_retries {
        if attempt > 0 {
            sleep(interval).await;
            interval = std::cmp::min(
                Duration::from_secs_f64(interval.as_secs_f64() * options.multiplier),
                options.max_interval,
            );
        }

        match operation().await {
            Ok(result) => return Ok(result),
            Err(error) => {
                // Check if the error is retryable before continuing
                if !is_retryable_error(&error) {
                    return Err(error);
                }

                last_error = Some(error);
                if attempt == options.max_retries {
                    break;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| CrosstacheError::unknown("Retry failed with no error")))
}

// TODO: Add context-aware cancellation support
// TODO: Add configurable retry conditions
// TODO: Add retry policy abstraction
