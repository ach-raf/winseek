//! Best match: how useful a name result is for what was typed.
//!
//! One score per result. *How the typed word sits in the name* picks a class, and the class is
//! most of the score: the four class bases are 300 apart and everything else together moves a
//! result by less than 200, so no combination of the smaller weights lifts a worse class over a
//! better one.
//!
//! | class  | the typed word is...                                   | for `awb`                         |
//! |--------|---------------------------------------------------------|-----------------------------------|
//! | Exact  | the whole name (a file: the name without its extension) | `awb`, `awb.xlsx`                  |
//! | Prefix | the start of the name                                   | `awb notes.docx`, `AWBCache.bin`   |
//! | Word   | the start of a word inside the name                     | `notes 2024_awb.docx`, `MyAwbFile` |
//! | Part   | inside a word                                           | `017 Drawbar Organ.fst`            |
//!
//! With several words typed, the weakest one decides. Inside a class the smaller weights order
//! the rest: folders before files, then recent over old, shallow over deep, a build / cache /
//! temp folder above costs, and a long name costs a little (the typed letters are a smaller
//! share of it). Apps and settings pages keep the launcher's own ranking (its match score plus
//! how often and how recently each was launched); this orders the files and folders.

use std::time::{SystemTime, UNIX_EPOCH};

/// How the typed word sits in a name, weakest to strongest (the order of [`RankWeights::class`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchClass {
    Part,
    Word,
    Prefix,
    Exact,
}

impl MatchClass {
    pub fn index(self) -> usize {
        match self {
            MatchClass::Part => 0,
            MatchClass::Word => 1,
            MatchClass::Prefix => 2,
            MatchClass::Exact => 3,
        }
    }
}

/// Folders almost nothing wanted is in: build output, downloaded dependencies, version-control
/// internals, caches, temp. A result below one of them costs [`RankWeights::noise`]. Matched on
/// the folder's whole name, any case. Generic English words are left out on purpose (`bin`,
/// `cache`, `Temp`, `Release`): too many real working folders are called that.
const NOISE_DIRS: &[&[u8]] = &[
    b"$Recycle.Bin",
    b".git",
    b".hg",
    b".svn",
    b".idea",
    b".vs",
    b".gradle",
    b".nuget",
    b".tox",
    b".venv",
    b".cache",
    b".mypy_cache",
    b".pytest_cache",
    b".ruff_cache",
    b"__pycache__",
    b"AppData",
    b"build",
    b"CMakeFiles",
    b"cvs",
    b"dist",
    b"node_modules",
    b"obj",
    b"site-packages",
    b"target",
    b"venv",
    b"WinSxS",
];

/// Is the folder `name` one of [`NOISE_DIRS`]? (Called once per folder per query: the name index
/// memoizes the verdict, so the short list is not worth a table.)
pub fn is_noise_dir(name: &[u8]) -> bool {
    NOISE_DIRS.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// 1601 -> 1970 in 100 ns units (the store keeps raw FILETIMEs).
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
const DAY_FT: i128 = 86_400 * 10_000_000;

/// The clock now, as the index stores modification times (FILETIME).
pub fn now_filetime() -> i64 {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    FILETIME_UNIX_EPOCH + ns / 100
}

/// Whole days between `mtime` and `now` (both FILETIMEs); a time in the future is 0.
pub fn age_days(now_ft: i64, mtime_ft: i64) -> u32 {
    ((now_ft as i128 - mtime_ft as i128).max(0) / DAY_FT) as u32
}

/// What one result is, for [`rank_score`]: how each typed word sits in its name and the facts
/// the smaller weights read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankFacts<'a> {
    /// The class of each typed word that sits in the name (empty: nothing was typed to rank by).
    pub classes: &'a [MatchClass],
    /// Bytes of the name as written, and of those typed words together.
    pub name_len: usize,
    pub typed_len: usize,
    pub is_dir: bool,
    /// Folders above it, the volume root not counted (`C:\a.txt` is 0).
    pub depth: u16,
    /// One of them is a build / cache / temp folder ([`NOISE_DIRS`]).
    pub noisy: bool,
    /// Whole days since it last changed; `None` when the index has no times.
    pub age_days: Option<u32>,
}

/// Younger than this many days earns each [`RankWeights::recency`] bonus: today, this week,
/// this month, this quarter, this year. Older earns nothing.
pub const RECENCY_DAYS: [u32; 5] = [1, 7, 30, 90, 365];

/// The weights of [`rank_score`]. Every class base stands at least
/// `(|smallest weight| summed) * 2` above the next, which is what keeps the classes apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankWeights {
    /// Base score by [`MatchClass`]: Part, Word, Prefix, Exact.
    pub class: [i64; 4],
    /// A folder, before the files of its class.
    pub folder: i64,
    /// Changed within [`RECENCY_DAYS`], oldest band last. The shape follows the re-finding
    /// studies (Dumais et al. 2003: of the items people reopened, 22% had been seen within the
    /// week, 46% within the month, 89% within the year): a fast first drop, then a slow one.
    /// The newest band stays just under `folder`, so at the same depth a folder still leads.
    pub recency: [i64; 5],
    /// Cost per folder of depth, and the most levels that cost.
    pub depth_step: i64,
    pub depth_max: u16,
    /// Below a folder of [`NOISE_DIRS`].
    pub noise: i64,
    /// Every `length_bytes` of name beyond the typed letters costs `length_cost`, down to `length_max`.
    pub length_bytes: i64,
    pub length_cost: i64,
    pub length_max: i64,
}

impl RankWeights {
    pub const DEFAULT: RankWeights = RankWeights {
        class: [100, 400, 700, 1000],
        folder: 30,
        recency: [28, 24, 18, 10, 4],
        depth_step: -2,
        depth_max: 15,
        noise: -60,
        length_bytes: 4,
        length_cost: -1,
        length_max: -20,
    };
}

/// The lowest score any result can have below its class base (every cost at its most, no bonus).
pub const SCORE_MIN: i64 = -110;
/// Added to a score before it is packed into a [`rank_key`], so it is never negative and
/// leaves the top bits free (every score is at most 1058, so the sum stays under 2^11).
const SCORE_FLOOR: i64 = 256;

/// The score of one result: its class base plus the smaller weights (see the module notes).
pub fn rank_score(f: &RankFacts) -> i64 {
    let w = &RankWeights::DEFAULT;
    let class = f
        .classes
        .iter()
        .copied()
        .min()
        .map_or(0, |c| w.class[c.index()]);
    let recency = match f.age_days {
        Some(d) if d < RECENCY_DAYS[0] => w.recency[0],
        Some(d) if d < RECENCY_DAYS[1] => w.recency[1],
        Some(d) if d < RECENCY_DAYS[2] => w.recency[2],
        Some(d) if d < RECENCY_DAYS[3] => w.recency[3],
        Some(d) if d < RECENCY_DAYS[4] => w.recency[4],
        _ => 0,
    };
    let depth = i64::from(f.depth.min(w.depth_max)) * w.depth_step;
    let noise = if f.noisy { w.noise } else { 0 };
    let extra = f.name_len.saturating_sub(f.typed_len) as i64;
    let length = (extra / w.length_bytes * w.length_cost).max(w.length_max);
    class + if f.is_dir { w.folder } else { 0 } + recency + depth + noise + length
}

/// Sort key of one result for a list read best first (the biggest key first): the score, then
/// the name A to Z, then the slot, so equal results keep one stable order while paging. The two
/// tie-breaks are stored upside down (a smaller rank / slot makes a bigger key).
pub fn rank_key(score: i64, name_rank: u32, slot: u32) -> i64 {
    const LOW: u64 = (1 << 26) - 1;
    let s = (score + SCORE_FLOOR) as u64;
    debug_assert!(s < 1 << 11, "score {score} does not fit its key field");
    let low = |v: u32| LOW - u64::from(v.min(LOW as u32));
    ((s << 52) | (low(name_rank) << 26) | low(slot)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn facts<'a>(classes: &'a [MatchClass]) -> RankFacts<'a> {
        RankFacts {
            classes,
            name_len: 3,
            typed_len: 3,
            is_dir: false,
            depth: 0,
            noisy: false,
            age_days: None,
        }
    }
    const E: MatchClass = MatchClass::Exact;
    const P: MatchClass = MatchClass::Prefix;
    const W: MatchClass = MatchClass::Word;
    const T: MatchClass = MatchClass::Part;

    #[test]
    fn the_class_decides_and_no_smaller_weight_can_undo_it() {
        // The weakest possible result of a class still beats the strongest of the class below:
        // every cost at its most vs every bonus at its most.
        let worst = |c: MatchClass| {
            rank_score(&RankFacts {
                depth: u16::MAX,
                noisy: true,
                age_days: Some(u32::MAX),
                name_len: 4000,
                ..facts(&[c])
            })
        };
        let best = |c: MatchClass| {
            rank_score(&RankFacts {
                is_dir: true,
                depth: 0,
                age_days: Some(0),
                ..facts(&[c])
            })
        };
        for pair in [(E, P), (P, W), (W, T)] {
            assert!(
                worst(pair.0) > best(pair.1),
                "worst {:?} {} must beat best {:?} {}",
                pair.0,
                worst(pair.0),
                pair.1,
                best(pair.1)
            );
        }
        assert_eq!(
            (best(E), worst(E)),
            (
                1058,
                SCORE_MIN + RankWeights::DEFAULT.class[MatchClass::Exact.index()]
            )
        );
    }

    #[test]
    fn with_several_words_the_weakest_decides() {
        assert_eq!(rank_score(&facts(&[E, P])), rank_score(&facts(&[P])));
        assert_eq!(rank_score(&facts(&[W, E, E])), rank_score(&facts(&[W])));
    }

    #[test]
    fn folders_come_before_files_of_their_class() {
        let file = rank_score(&RankFacts {
            age_days: Some(0),
            ..facts(&[E])
        });
        let dir = rank_score(&RankFacts {
            is_dir: true,
            age_days: Some(4000),
            ..facts(&[E])
        });
        assert!(
            dir > file,
            "an old folder {dir} must come before a file changed today {file}"
        );
    }

    #[test]
    fn the_smaller_weights_each_move_a_result() {
        let base = rank_score(&facts(&[W]));
        let w = &RankWeights::DEFAULT;
        assert_eq!(
            rank_score(&RankFacts {
                age_days: Some(0),
                ..facts(&[W])
            }),
            base + w.recency[0]
        );
        assert_eq!(
            rank_score(&RankFacts {
                age_days: Some(400),
                ..facts(&[W])
            }),
            base
        );
        assert_eq!(
            rank_score(&RankFacts {
                depth: 3,
                ..facts(&[W])
            }),
            base + 3 * w.depth_step
        );
        assert_eq!(
            rank_score(&RankFacts {
                depth: u16::MAX,
                ..facts(&[W])
            }),
            base + i64::from(w.depth_max) * w.depth_step
        );
        assert_eq!(
            rank_score(&RankFacts {
                noisy: true,
                ..facts(&[W])
            }),
            base + w.noise
        );
        assert_eq!(
            rank_score(&RankFacts {
                name_len: 23,
                ..facts(&[W])
            }),
            base + 5 * w.length_cost
        );
        assert_eq!(
            rank_score(&RankFacts {
                name_len: 4000,
                ..facts(&[W])
            }),
            base + w.length_max
        );
        // a name shorter than the typed letters (a word matched through case folding): no cost
        assert_eq!(
            rank_score(&RankFacts {
                name_len: 1,
                ..facts(&[W])
            }),
            base
        );
    }

    #[test]
    fn recency_bands_follow_the_days() {
        let at = |d: u32| {
            rank_score(&RankFacts {
                age_days: Some(d),
                ..facts(&[P])
            })
        };
        let base = rank_score(&facts(&[P]));
        let w = &RankWeights::DEFAULT;
        assert_eq!(at(0), base + w.recency[0]);
        assert_eq!(at(1), base + w.recency[1]);
        assert_eq!(at(6), base + w.recency[1]);
        assert_eq!(at(7), base + w.recency[2]);
        assert_eq!(at(30), base + w.recency[3]);
        assert_eq!(at(90), base + w.recency[4]);
        assert_eq!(at(364), base + w.recency[4]);
        assert_eq!(at(365), base);
    }

    #[test]
    fn age_and_clock_helpers() {
        const DAY: i64 = 86_400 * 10_000_000;
        let now = FILETIME_UNIX_EPOCH + 1_700_000 * DAY;
        assert_eq!(age_days(now, now), 0);
        assert_eq!(age_days(now, now - 10 * DAY - 1), 10);
        assert_eq!(
            age_days(now, now + 5 * DAY),
            0,
            "a time in the future is not negative age"
        );
        // the clock is after 2023-11-14 (1,700,000,000 s after 1970)
        assert!(now_filetime() > FILETIME_UNIX_EPOCH + 1_700_000_000 * 10_000_000);
    }

    #[test]
    fn keys_order_by_score_then_name_then_slot() {
        let best = rank_key(1000, 7, 90);
        assert!(best > rank_key(999, 0, 0), "score first");
        assert!(rank_key(500, 1, 90) > rank_key(500, 2, 1), "then the name");
        assert!(rank_key(500, 1, 1) > rank_key(500, 1, 2), "then the slot");
        assert!(
            rank_key(SCORE_MIN, u32::MAX, u32::MAX) >= 0,
            "every key is positive"
        );
    }

    #[test]
    fn noise_folders_by_whole_name_any_case() {
        for n in [
            "target",
            "TARGET",
            "node_modules",
            ".Git",
            "AppData",
            "$RECYCLE.BIN",
            "site-packages",
            "WinSxS",
        ] {
            assert!(is_noise_dir(n.as_bytes()), "{n}");
        }
        for n in [
            "",
            "bin",
            "cache",
            "Temp",
            "Release",
            "targets",
            "my target",
            "mytarget",
            "git",
        ] {
            assert!(!is_noise_dir(n.as_bytes()), "{n}");
        }
    }
}
