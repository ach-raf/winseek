//! winseek-usn: USN journal record parsing, rename pairing and monitor loop.
//!
//! * [`parse_usn_buffer`] / [`classify`]: pure and unit-tested.
//! * [`RenamePairer`]: turns raw records into [`Change`]s.
//! * `monitor` (Windows ioctls, compile-only): [`query_journal`],
//!   [`read_journal`], [`run_monitor`].
//!
//! Handle rule: monitor handles come from `winseek_ntfs::open_volume_for_usn`
//! (with NO_BUFFERING); scan handles do not use it.

mod monitor;
mod pairer;
mod parse;

pub use monitor::{
    query_journal, read_journal, run_monitor, JournalCursor, JournalInfo, MonitorConfig, RecordVersion,
    MonitorError, MonitorExit, ReadOutcome, DEFAULT_BUF, FSCTL_QUERY_USN_JOURNAL,
    FSCTL_READ_USN_JOURNAL,
};
pub use pairer::{Change, RenamePairer};
pub use parse::{classify, parse_usn_buffer, Op, UsnError, UsnRecord};
pub use parse::{
    USN_REASON_BASIC_INFO_CHANGE, USN_REASON_CLOSE, USN_REASON_DATA_EXTEND,
    USN_REASON_DATA_OVERWRITE, USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE,
    USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
};
