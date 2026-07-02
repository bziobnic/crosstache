//! Shared pagination helpers for CLI list commands.

use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;

/// Runtime pagination settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pagination {
    pub page: usize,
    pub page_size: Option<usize>,
}

impl Pagination {
    pub fn from_args(page: Option<usize>, page_size: Option<usize>) -> Result<Self> {
        if matches!(page, Some(0)) {
            return Err(CrosstacheError::invalid_argument(
                "--page must be greater than zero",
            ));
        }

        if matches!(page_size, Some(0)) {
            return Err(CrosstacheError::invalid_argument(
                "--page-size must be greater than zero",
            ));
        }

        if page.is_some() && page_size.is_none() {
            return Err(CrosstacheError::invalid_argument(
                "--page requires --page-size",
            ));
        }

        Ok(Self {
            page: page.unwrap_or(1),
            page_size,
        })
    }

    pub fn first_page_with_size(page_size: Option<usize>) -> Result<Self> {
        Self::from_args(None, page_size)
    }
}

/// A paginated result slice plus display metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total_items: usize,
    pub page: usize,
    pub page_size: Option<usize>,
    pub total_pages: Option<usize>,
    pub start_index_1_based: Option<usize>,
    pub end_index_1_based: Option<usize>,
}

impl<T> Page<T> {
    pub fn human_summary(&self, noun_singular: &str, noun_plural: &str) -> Option<String> {
        let page_size = self.page_size?;
        let total_pages = self.total_pages.unwrap_or(0);
        let noun = if self.total_items == 1 {
            noun_singular
        } else {
            noun_plural
        };

        if self.total_items == 0 || self.items.is_empty() {
            return Some(format!(
                "Showing 0 of {} {} — page {} of {}",
                self.total_items, noun, self.page, total_pages
            ));
        }

        Some(format!(
            "Showing {}-{} of {} {} — page {} of {} (page size {})",
            self.start_index_1_based.unwrap_or(0),
            self.end_index_1_based.unwrap_or(0),
            self.total_items,
            noun,
            self.page,
            total_pages,
            page_size
        ))
    }
}

/// Return a page of `items`. When pagination is disabled, all items are returned.
pub fn paginate_slice<T: Clone>(items: &[T], pagination: Pagination) -> Page<T> {
    let total_items = items.len();

    let Some(page_size) = pagination.page_size else {
        return Page {
            items: items.to_vec(),
            total_items,
            page: 1,
            page_size: None,
            total_pages: None,
            start_index_1_based: if total_items == 0 { None } else { Some(1) },
            end_index_1_based: if total_items == 0 {
                None
            } else {
                Some(total_items)
            },
        };
    };

    let total_pages = if total_items == 0 {
        0
    } else {
        total_items.div_ceil(page_size)
    };

    let start = (pagination.page - 1).saturating_mul(page_size);
    let end = usize::min(start.saturating_add(page_size), total_items);
    let page_items = if start >= total_items {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };

    Page {
        start_index_1_based: if page_items.is_empty() {
            None
        } else {
            Some(start + 1)
        },
        end_index_1_based: if page_items.is_empty() {
            None
        } else {
            Some(end)
        },
        items: page_items,
        total_items,
        page: pagination.page,
        page_size: Some(page_size),
        total_pages: Some(total_pages),
    }
}

/// Return footer text for human-readable output formats.
pub fn pagination_footer_text<T>(
    page: &Page<T>,
    noun_singular: &str,
    noun_plural: &str,
    output_format: OutputFormat,
) -> Option<String> {
    let human_output = matches!(
        output_format.resolve_for_stdout(),
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    human_output
        .then(|| page.human_summary(noun_singular, noun_plural))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_pagination_returns_all_items() {
        let page = paginate_slice(&[1, 2, 3], Pagination::from_args(None, None).unwrap());
        assert_eq!(page.items, vec![1, 2, 3]);
        assert!(page.page_size.is_none());
    }

    #[test]
    fn first_page_returns_expected_items() {
        let page = paginate_slice(
            &[1, 2, 3, 4, 5],
            Pagination::from_args(None, Some(2)).unwrap(),
        );
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.total_pages, Some(3));
        assert_eq!(page.start_index_1_based, Some(1));
        assert_eq!(page.end_index_1_based, Some(2));
    }

    #[test]
    fn middle_page_returns_expected_items() {
        let page = paginate_slice(
            &[1, 2, 3, 4, 5],
            Pagination::from_args(Some(2), Some(2)).unwrap(),
        );
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.start_index_1_based, Some(3));
        assert_eq!(page.end_index_1_based, Some(4));
    }

    #[test]
    fn page_past_end_returns_empty_items_with_total_metadata() {
        let page = paginate_slice(&[1, 2, 3], Pagination::from_args(Some(3), Some(2)).unwrap());
        assert!(page.items.is_empty());
        assert_eq!(page.total_items, 3);
        assert_eq!(page.total_pages, Some(2));
    }

    #[test]
    fn validates_page_and_page_size() {
        assert!(Pagination::from_args(Some(0), Some(10)).is_err());
        assert!(Pagination::from_args(Some(1), Some(0)).is_err());
        assert!(Pagination::from_args(Some(1), None).is_err());
    }

    #[test]
    fn human_summary_is_plural_aware() {
        let one = paginate_slice(&[1], Pagination::from_args(None, Some(5)).unwrap());
        assert_eq!(
            one.human_summary("secret", "secrets").unwrap(),
            "Showing 1-1 of 1 secret — page 1 of 1 (page size 5)"
        );

        let many = paginate_slice(&[1, 2, 3], Pagination::from_args(None, Some(2)).unwrap());
        assert_eq!(
            many.human_summary("secret", "secrets").unwrap(),
            "Showing 1-2 of 3 secrets — page 1 of 2 (page size 2)"
        );

        let empty = paginate_slice(&[] as &[i32], Pagination::from_args(None, Some(2)).unwrap());
        assert_eq!(
            empty.human_summary("entry", "entries").unwrap(),
            "Showing 0 of 0 entries — page 1 of 0"
        );
    }
}
