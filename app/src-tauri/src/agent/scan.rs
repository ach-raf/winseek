//! Date/size range scan over a paged name search: the same walk the popup does in `App.tsx`
//! (`SCAN_PAGE` 2000, cap 30000), extracted as a pure function so the agent gets identical
//! results. The source sorts by date (newest first) or size (biggest first); we stop as soon
//! as a page is past the range.

pub const SCAN_PAGE: usize = 2000;
pub const SCAN_MAX: usize = 30_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    /// Unix ms: keep `from <= mtime < to`.
    Date { from_ms: i64, to_ms: i64 },
    /// Bytes; either side may be unbounded.
    Size { min: Option<u64>, max: Option<u64> },
}

/// One page-walk. `page(offset, limit)` returns `(total, hits)`; `mtime`/`size` read a hit;
/// `extra` is an additional predicate (the popup can filter date *and* size at once). Returns
/// `(total in range, kept hits, truncated)`; `limit`/`offset` apply to the filtered result,
/// not to the scan window.
pub fn scan<T>(
    mut page: impl FnMut(usize, usize) -> Result<(u64, Vec<T>), String>,
    bound: &Bound,
    extra: &dyn Fn(&T) -> bool,
    mtime: impl Fn(&T) -> i64,
    size: impl Fn(&T) -> Option<u64>,
    limit: usize,
    offset: usize,
) -> Result<(u64, Vec<T>, bool), String> {
    let mut in_range: Vec<T> = Vec::new();
    let mut scanned = 0usize;
    let mut page_offset = 0usize;
    let truncated;
    loop {
        let (total, hits) = page(page_offset, SCAN_PAGE)?;
        let mut stop = false;
        for h in hits {
            scanned += 1;
            let inb = match bound {
                Bound::Date { from_ms, to_ms } => {
                    let t = mtime(&h);
                    t >= *from_ms && t < *to_ms
                }
                Bound::Size { min, max } => match size(&h) {
                    // Sorted biggest-first: a missing size cannot be placed in the range.
                    None => false,
                    Some(s) => min.is_none_or(|m| s >= m) && max.is_none_or(|m| s <= m),
                },
            } && extra(&h);
            if inb {
                in_range.push(h);
                continue;
            }
            // Sorted newest-first / biggest-first: once below `from`/`min` everything after is too.
            let past = match bound {
                Bound::Date { from_ms, .. } => mtime(&h) < *from_ms,
                Bound::Size { min, .. } => size(&h).is_some_and(|s| min.is_some_and(|m| s < m)),
            };
            if past {
                stop = true;
                break;
            }
        }
        page_offset += SCAN_PAGE;
        if stop || page_offset as u64 >= total || scanned >= SCAN_MAX {
            truncated = !stop && scanned >= SCAN_MAX && (page_offset as u64) < total;
            break;
        }
    }
    let total_in_range = in_range.len() as u64;
    let kept = in_range.into_iter().skip(offset).take(limit).collect();
    Ok((total_in_range, kept, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pager(total: usize, mtimes: Vec<i64>) -> impl FnMut(usize, usize) -> Result<(u64, Vec<i64>), String> {
        move |offset, limit| {
            let end = (offset + limit).min(total);
            if offset >= total {
                return Ok((total as u64, vec![]));
            }
            Ok((total as u64, mtimes[offset..end].to_vec()))
        }
    }

    const DAY: i64 = 86_400_000;

    #[test]
    fn stops_past_the_range() {
        // Newest first: 10, 9, ..., 1 days ago. Query: last 3 days -> only ages 0..3.
        let mtimes: Vec<i64> = (0..50).map(|i| -i * DAY).collect();
        let (total, kept, truncated) = scan(
            pager(50, mtimes),
            &Bound::Date { from_ms: -3 * DAY, to_ms: DAY },
            &|_| true,
            |m| *m,
            |_| None,
            200,
            0,
        )
        .unwrap();
        assert_eq!(total, 4); // ages 0,1,2,3 days
        assert_eq!(kept, vec![0, -DAY, -2 * DAY, -3 * DAY]);
        assert!(!truncated);
    }

    #[test]
    fn cap_sets_truncated() {
        // Everything inside the range and more rows than the cap: the walk must stop at
        // SCAN_MAX with truncated, not walk all 40 000.
        let mtimes: Vec<i64> = (0..40_000).map(|i| -i64::from(i) * 1000).collect();
        let (total, kept, truncated) = scan(
            pager(40_000, mtimes),
            &Bound::Date { from_ms: -5 * DAY, to_ms: DAY },
            &|_| true,
            |m| *m,
            |_| None,
            10,
            0,
        )
        .unwrap();
        assert_eq!(total, 30_000);
        assert_eq!(kept.len(), 10);
        assert!(truncated);
    }

    #[test]
    fn offset_and_limit_apply_after_filtering() {
        let mtimes: Vec<i64> = (0..20).map(|i| -(i as i64) * DAY).collect();
        let (_, kept, _) = scan(
            pager(20, mtimes),
            &Bound::Date { from_ms: -10 * DAY, to_ms: DAY },
            &|_| true,
            |m| *m,
            |_| None,
            3,
            2,
        )
        .unwrap();
        assert_eq!(kept, vec![-2 * DAY, -3 * DAY, -4 * DAY]);
    }

    #[test]
    fn size_bounds() {
        let sizes: Vec<Option<u64>> = vec![Some(1000), Some(500), None, Some(100)];
        let (total, _, truncated) = scan(
            move |offset, limit| {
                let end = (offset + limit).min(4);
                Ok((4, sizes[offset..end].to_vec()))
            },
            &Bound::Size { min: Some(200), max: None },
            &|_| true,
            |_| 0,
            |s| *s,
            10,
            0,
        )
        .unwrap();
        assert_eq!(total, 2);
        assert!(!truncated);
    }
}
