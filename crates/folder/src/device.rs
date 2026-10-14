//! `CM_Register_Notification` (cfgmgr32): volume arrival/removal and per-handle events.
//!
//! Unlike `RegisterDeviceNotification` this needs neither a window nor a service status
//! handle, so it works the same in the SYSTEM service and in `--console` mode. Callbacks run on
//! a system thread-pool thread; do not call [`Registration`]'s drop (i.e. unregister) from
//! inside a callback (it waits for running callbacks).

#![allow(non_snake_case)]

use crate::sys::HANDLE;
use std::ffi::c_void;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};

const CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE: u32 = 0;
const CM_NOTIFY_FILTER_TYPE_DEVICEHANDLE: u32 = 1;

const ACTION_DEVICEINTERFACEARRIVAL: u32 = 0;
const ACTION_DEVICEINTERFACEREMOVAL: u32 = 1;
const ACTION_DEVICEQUERYREMOVE: u32 = 2;
const ACTION_DEVICEQUERYREMOVEFAILED: u32 = 3;
const ACTION_DEVICEREMOVEPENDING: u32 = 4;
const ACTION_DEVICEREMOVECOMPLETE: u32 = 5;

/// `{53F5630D-B6BF-11D0-94F2-00A0C91EFB8B}` in in-memory (little-endian GUID) byte order.
const GUID_DEVINTERFACE_VOLUME: [u8; 16] = [
    0x0d, 0x63, 0xf5, 0x53, 0xbf, 0xb6, 0xd0, 0x11, 0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b,
];

/// `CM_NOTIFY_FILTER`: header (16 bytes) + union sized by `InstanceId[MAX_DEVICE_ID_LEN = 200]`
/// WCHARs = 400 bytes (8-aligned because the handle member is pointer sized).
#[repr(C)]
struct CmNotifyFilter {
    cb_size: u32,
    flags: u32,
    filter_type: u32,
    reserved: u32,
    u: [u64; 50],
}

const _: () = assert!(std::mem::size_of::<CmNotifyFilter>() == 416);

type CmCallback = extern "system" fn(isize, *mut c_void, u32, *const c_void, u32) -> u32;

#[link(name = "cfgmgr32")]
extern "system" {
    fn CM_Register_Notification(
        pFilter: *const CmNotifyFilter,
        pContext: *mut c_void,
        pCallback: CmCallback,
        pNotifyContext: *mut isize,
    ) -> u32;
    fn CM_Unregister_Notification(NotifyContext: isize) -> u32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleEvent {
    QueryRemove,
    QueryRemoveFailed,
    RemovePending,
    RemoveComplete,
    Other,
}

/// A live notification registration; dropping it unregisters (waiting for in-flight callbacks)
/// and then frees the callback closure.
pub struct Registration {
    handle: isize,
    ctx: *mut c_void,
    free: unsafe fn(*mut c_void),
}

// SAFETY: the HCMNOTIFICATION is just an opaque token and the closure behind `ctx` is
// `Send + Sync`; unregistering from any thread is allowed.
unsafe impl Send for Registration {}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful CM_Register_Notification; unregistering first
        // guarantees no callback is running or will run, so freeing `ctx` afterwards is sound.
        unsafe {
            CM_Unregister_Notification(self.handle);
            (self.free)(self.ctx);
        }
    }
}

type IfaceFn = Box<dyn Fn() + Send + Sync>;
type HandleFn = Box<dyn Fn(HandleEvent) + Send + Sync>;

unsafe fn free_iface(p: *mut c_void) {
    // SAFETY: caller passes the pointer made by `Box::into_raw(Box::new(IfaceFn))`.
    drop(unsafe { Box::from_raw(p as *mut IfaceFn) });
}

unsafe fn free_handle(p: *mut c_void) {
    // SAFETY: caller passes the pointer made by `Box::into_raw(Box::new(HandleFn))`.
    drop(unsafe { Box::from_raw(p as *mut HandleFn) });
}

extern "system" fn iface_cb(_h: isize, ctx: *mut c_void, action: u32, _data: *const c_void, _size: u32) -> u32 {
    if ctx.is_null() {
        return 0;
    }
    if action == ACTION_DEVICEINTERFACEARRIVAL || action == ACTION_DEVICEINTERFACEREMOVAL {
        // SAFETY: ctx is the live IfaceFn box owned by the Registration (freed only after unregister).
        let f = unsafe { &*(ctx as *const IfaceFn) };
        let _ = catch_unwind(AssertUnwindSafe(f));
    }
    0 // ERROR_SUCCESS
}

extern "system" fn handle_cb(_h: isize, ctx: *mut c_void, action: u32, _data: *const c_void, _size: u32) -> u32 {
    if ctx.is_null() {
        return 0;
    }
    let ev = match action {
        ACTION_DEVICEQUERYREMOVE => HandleEvent::QueryRemove,
        ACTION_DEVICEQUERYREMOVEFAILED => HandleEvent::QueryRemoveFailed,
        ACTION_DEVICEREMOVEPENDING => HandleEvent::RemovePending,
        ACTION_DEVICEREMOVECOMPLETE => HandleEvent::RemoveComplete,
        _ => HandleEvent::Other,
    };
    // SAFETY: ctx is the live HandleFn box owned by the Registration.
    let f = unsafe { &*(ctx as *const HandleFn) };
    let _ = catch_unwind(AssertUnwindSafe(|| f(ev)));
    0 // ERROR_SUCCESS: allow the removal
}

fn register(filter: &CmNotifyFilter, ctx: *mut c_void, cb: CmCallback, free: unsafe fn(*mut c_void)) -> io::Result<Registration> {
    let mut h: isize = 0;
    // SAFETY: `filter` is a fully initialised 416-byte CM_NOTIFY_FILTER; `ctx` stays valid until
    // the Registration is dropped; `h` receives the notification handle.
    let cr = unsafe { CM_Register_Notification(filter, ctx, cb, &mut h) };
    if cr != 0 {
        // SAFETY: registration failed so no callback can reference `ctx`; free it now.
        unsafe { free(ctx) };
        return Err(io::Error::other(format!("CM_Register_Notification CR {cr}")));
    }
    Ok(Registration { handle: h, ctx, free })
}

/// Volume interface arrival/removal; calls `on_change` for each.
pub fn watch_volume_interfaces(on_change: Box<dyn Fn() + Send + Sync>) -> io::Result<Registration> {
    let mut f = CmNotifyFilter {
        cb_size: std::mem::size_of::<CmNotifyFilter>() as u32,
        flags: 0,
        filter_type: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        reserved: 0,
        u: [0; 50],
    };
    f.u[0] = u64::from_le_bytes(GUID_DEVINTERFACE_VOLUME[..8].try_into().unwrap());
    f.u[1] = u64::from_le_bytes(GUID_DEVINTERFACE_VOLUME[8..].try_into().unwrap());
    let ctx = Box::into_raw(Box::new(on_change)) as *mut c_void;
    register(&f, ctx, iface_cb, free_iface)
}

/// Events for one raw handle (`CM_NOTIFY_FILTER_TYPE_DEVICEHANDLE`). The callback must return
/// quickly; for `QueryRemove` it may block (bounded) while the owner closes the handle.
pub fn watch_handle(raw: HANDLE, on_event: Box<dyn Fn(HandleEvent) + Send + Sync>) -> io::Result<Registration> {
    let mut f = CmNotifyFilter {
        cb_size: std::mem::size_of::<CmNotifyFilter>() as u32,
        flags: 0,
        filter_type: CM_NOTIFY_FILTER_TYPE_DEVICEHANDLE,
        reserved: 0,
        u: [0; 50],
    };
    f.u[0] = raw as u64; // hTarget
    let ctx = Box::into_raw(Box::new(on_event)) as *mut c_void;
    register(&f, ctx, handle_cb, free_handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use crate::watch::DirWatch;

    #[test]
    fn filter_layout() {
        assert_eq!(std::mem::size_of::<CmNotifyFilter>(), 416);
        assert_eq!(std::mem::offset_of!(CmNotifyFilter, u), 16);
    }

    #[test]
    fn volume_interfaces_register_and_drop() {
        let r = watch_volume_interfaces(Box::new(|| {})).unwrap();
        drop(r);
        let r1 = watch_volume_interfaces(Box::new(|| {})).unwrap();
        let r2 = watch_volume_interfaces(Box::new(|| {})).unwrap();
        drop(r1);
        drop(r2);
    }

    #[test]
    fn handle_registers_and_drops() {
        let t = TempDir::new("devhandle");
        let w = DirWatch::open(t.s()).unwrap();
        let r = watch_handle(w.raw_handle(), Box::new(|_| {})).unwrap();
        drop(r);
        let r = watch_handle(w.raw_handle(), Box::new(|_| {})).unwrap();
        drop(w); // handle closed before the registration is dropped: must not crash
        drop(r);
    }

    #[test]
    fn bad_handle_is_error() {
        assert!(watch_handle(0, Box::new(|_| {})).is_err());
    }
}
