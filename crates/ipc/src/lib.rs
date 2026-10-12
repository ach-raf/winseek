//! winseek-ipc: protocol between `searchd` (SYSTEM) and the user-level app.
//!
//! Framing: `u32` little-endian length prefix + JSON body, max 16 MiB.
//! Transport: Windows named pipe [`PIPE_NAME`] (see [`pipe`], Windows only).
//!
//! Note: pipe handles are synchronous, so reads and writes on one connection
//! are serialised by the OS. Use a dedicated connection for `Subscribe`.

pub mod frame;
#[cfg(windows)]
pub mod pipe;
pub mod proto;

pub use frame::{read_frame, read_message, write_frame, write_message, MAX_FRAME};
pub use proto::*;

/// Well-known pipe name of the searchd service.
pub const PIPE_NAME: &str = r"\\.\pipe\winseek-searchd";
