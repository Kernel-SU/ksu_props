//! Bionic `__system_property_*` API wrapper via `dlsym`.
//!
//! This module dynamically loads Android bionic's system property functions at
//! runtime and exposes a safe Rust API.  It implements the same dual-channel
//! write logic as Magisk's resetprop:
//!
//! - `ro.*` properties bypass `property_service` and are modified directly via
//!   `__system_property_update` / `__system_property_add` (shared-memory mmap).
//! - Other properties go through `__system_property_set` which internally
//!   connects to init's `property_service` socket.

use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;
use std::time::Duration;

use prop_rs::{PersistentPropError, PROP_VALUE_MAX};

use crate::persist;

// ── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SysPropError {
    /// A required bionic symbol was not found via dlsym.
    SymbolNotFound(&'static str),
    /// `__system_properties_init` returned a non-zero error code.
    InitFailed(c_int),
    /// `__system_property_set` returned a non-zero error code.
    SetFailed(c_int),
    /// `__system_property_add` returned a non-zero error code.
    AddFailed(c_int),
    /// `__system_property_update` returned a non-zero error code.
    UpdateFailed(c_int),
    /// `__system_property_del` is unavailable (stock bionic without Magisk).
    DeleteUnavailable,
    /// The property key or value contains an interior NUL byte.
    InvalidCString(String),
    /// A persistent-property I/O operation failed.
    Persistent(PersistentPropError),
}

impl fmt::Display for SysPropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymbolNotFound(sym) => write!(f, "bionic symbol not found: {sym}"),
            Self::InitFailed(code) => write!(f, "__system_properties_init failed: {code}"),
            Self::SetFailed(code) => write!(f, "__system_property_set failed: {code}"),
            Self::AddFailed(code) => write!(f, "__system_property_add failed: {code}"),
            Self::UpdateFailed(code) => write!(f, "__system_property_update failed: {code}"),
            Self::DeleteUnavailable => write!(f, "__system_property_del not available"),
            Self::InvalidCString(s) => write!(f, "invalid C string: {s}"),
            Self::Persistent(e) => write!(f, "persistent property error: {e}"),
        }
    }
}

impl std::error::Error for SysPropError {}

impl From<PersistentPropError> for SysPropError {
    fn from(e: PersistentPropError) -> Self {
        Self::Persistent(e)
    }
}

pub type SysPropResult<T> = std::result::Result<T, SysPropError>;

// ── Opaque prop_info pointer ────────────────────────────────────────────────

/// Opaque handle to a bionic `prop_info` structure in shared memory.
#[derive(Debug, Clone, Copy)]
pub struct PropInfoPtr(*const c_void);

// Safety: prop_info lives in MAP_SHARED memory and is safe to access from any
// thread (bionic uses atomics for the serial protocol).
unsafe impl Send for PropInfoPtr {}
unsafe impl Sync for PropInfoPtr {}

impl PropInfoPtr {
    fn as_ptr(self) -> *const c_void {
        self.0
    }
}

// ── Function-pointer type aliases ───────────────────────────────────────────

type FnInit = unsafe extern "C" fn() -> c_int;
type FnFind = unsafe extern "C" fn(*const c_char) -> *const c_void;
type FnReadCallback = unsafe extern "C" fn(
    *const c_void, // pi
    Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, u32)>,
    *mut c_void, // cookie
);
type FnForEach = unsafe extern "C" fn(
    Option<unsafe extern "C" fn(*const c_void, *mut c_void)>,
    *mut c_void,
);
type FnSet = unsafe extern "C" fn(*const c_char, *const c_char) -> c_int;
type FnAdd = unsafe extern "C" fn(*const c_char, c_uint, *const c_char, c_uint) -> c_int;
type FnUpdate = unsafe extern "C" fn(*const c_void, *const c_char, c_uint) -> c_int;
type FnDel = unsafe extern "C" fn(*const c_void) -> c_int;
type FnSerial = unsafe extern "C" fn(*const c_void) -> u32;
type FnAreaSerial = unsafe extern "C" fn() -> u32;
type FnWait = unsafe extern "C" fn(
    *const c_void,         // pi (null = global)
    u32,                   // old serial
    *mut u32,              // new serial out
    *const libc::timespec, // timeout (null = infinite)
) -> bool;

// ── Loaded API singleton ────────────────────────────────────────────────────

struct BionicApi {
    // Required symbols
    find: FnFind,
    read_callback: FnReadCallback,
    for_each: FnForEach,
    set: FnSet,
    add: FnAdd,
    update: FnUpdate,
    serial: FnSerial,
    // Optional symbols (may not exist on all Android versions / bionic variants)
    del: Option<FnDel>,
    area_serial: Option<FnAreaSerial>,
    wait: Option<FnWait>,
}

static API: OnceLock<BionicApi> = OnceLock::new();

fn load_sym<T>(name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, cname.as_ptr()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

fn load_sym_required<T>(name: &'static str) -> SysPropResult<T> {
    load_sym(name).ok_or(SysPropError::SymbolNotFound(name))
}

fn api() -> &'static BionicApi {
    API.get().expect("sys_prop::init() must be called first")
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Load all bionic symbols and call `__system_properties_init`.
///
/// This must be called once before any other function in this module.
/// Subsequent calls are harmless no-ops.
pub fn init() -> SysPropResult<()> {
    if API.get().is_some() {
        return Ok(());
    }

    let init_fn: FnInit = load_sym_required("__system_properties_init")?;

    let bionic = BionicApi {
        find: load_sym_required("__system_property_find")?,
        read_callback: load_sym_required("__system_property_read_callback")?,
        for_each: load_sym_required("__system_property_foreach")?,
        set: load_sym_required("__system_property_set")?,
        add: load_sym_required("__system_property_add")?,
        update: load_sym_required("__system_property_update")?,
        serial: load_sym_required("__system_property_serial")?,
        del: load_sym("__system_property_del"),
        area_serial: load_sym("__system_property_area_serial"),
        wait: load_sym("__system_property_wait"),
    };

    let ret = unsafe { init_fn() };
    if ret != 0 {
        return Err(SysPropError::InitFailed(ret));
    }

    let _ = API.set(bionic);
    Ok(())
}

/// Find a property by name, returning an opaque handle.
pub fn find(key: &str) -> Option<PropInfoPtr> {
    let ckey = CString::new(key).ok()?;
    let pi = unsafe { (api().find)(ckey.as_ptr()) };
    if pi.is_null() {
        None
    } else {
        Some(PropInfoPtr(pi))
    }
}

/// Read the value of a property by name.
pub fn get(key: &str) -> Option<String> {
    let pi = find(key)?;
    read_value(pi)
}

/// Read the current value from a prop_info handle.
pub fn read(pi: PropInfoPtr) -> Option<(String, String)> {
    read_name_value(pi)
}

/// Get the serial number of a prop_info.
pub fn serial(pi: PropInfoPtr) -> u32 {
    unsafe { (api().serial)(pi.as_ptr()) }
}

/// Get the global area serial (if available).
pub fn area_serial() -> Option<u32> {
    api().area_serial.map(|f| unsafe { f() })
}

/// Iterate over all properties.
pub fn for_each(mut callback: impl FnMut(&str, &str)) {
    // We need a double indirection: for_each gives us pi, then we read_callback
    // to get name+value.
    struct Cookie<'a> {
        cb: &'a mut dyn FnMut(&str, &str),
        read_cb: FnReadCallback,
    }

    unsafe extern "C" fn iter_cb(pi: *const c_void, cookie: *mut c_void) {
        let c = &mut *(cookie as *mut Cookie);

        struct Inner {
            name: Option<String>,
            value: Option<String>,
        }
        unsafe extern "C" fn read_cb(
            ck: *mut c_void,
            name: *const c_char,
            value: *const c_char,
            _serial: u32,
        ) {
            let inner = &mut *(ck as *mut Inner);
            inner.name = Some(CStr::from_ptr(name).to_string_lossy().into_owned());
            inner.value = Some(CStr::from_ptr(value).to_string_lossy().into_owned());
        }

        let mut inner = Inner {
            name: None,
            value: None,
        };
        (c.read_cb)(pi, Some(read_cb), &mut inner as *mut _ as *mut c_void);
        if let (Some(n), Some(v)) = (inner.name, inner.value) {
            (c.cb)(&n, &v);
        }
    }

    let mut cookie = Cookie {
        cb: &mut callback,
        read_cb: api().read_callback,
    };
    unsafe {
        (api().for_each)(Some(iter_cb), &mut cookie as *mut _ as *mut c_void);
    }
}

/// Set a property value using Magisk's dual-channel logic.
///
/// - For `ro.*` properties: bypasses property_service socket and operates
///   directly on the shared-memory mmap via `__system_property_update` /
///   `__system_property_add`.  Long or expanding values are handled by
///   automatic delete+add.
/// - For other properties: uses `__system_property_set` which goes through
///   init's property_service socket (handles serial updates, triggers, etc.).
/// - For `persist.*` properties with `skip_svc`: manually persists to disk.
pub fn set(key: &str, value: &str, skip_svc: bool) -> SysPropResult<()> {
    let api = api();
    let ckey = make_cstring(key)?;
    let cval = make_cstring(value)?;

    let force_skip = skip_svc || key.starts_with("ro.");
    let mut pi = find(key);

    if force_skip {
        if let Some(info) = pi {
            let needs_long = value.len() >= PROP_VALUE_MAX;
            if is_long_value(info) || needs_long {
                // Delete without pruning, then re-add.
                delete_internal(info)?;
                pi = None;
            }
        }
    }

    match pi {
        Some(info) => {
            if force_skip {
                let ret = unsafe {
                    (api.update)(info.as_ptr(), cval.as_ptr(), value.len() as c_uint)
                };
                if ret != 0 {
                    return Err(SysPropError::UpdateFailed(ret));
                }
            } else {
                let ret = unsafe { (api.set)(ckey.as_ptr(), cval.as_ptr()) };
                if ret != 0 {
                    return Err(SysPropError::SetFailed(ret));
                }
            }
        }
        None => {
            if force_skip {
                let ret = unsafe {
                    (api.add)(
                        ckey.as_ptr(),
                        key.len() as c_uint,
                        cval.as_ptr(),
                        value.len() as c_uint,
                    )
                };
                if ret != 0 {
                    return Err(SysPropError::AddFailed(ret));
                }
            } else {
                let ret = unsafe { (api.set)(ckey.as_ptr(), cval.as_ptr()) };
                if ret != 0 {
                    return Err(SysPropError::SetFailed(ret));
                }
            }
        }
    }

    // Manually persist when bypassing property_service.
    if force_skip && key.starts_with("persist.") {
        persist::persist_set_prop(key, value)?;
    }

    Ok(())
}

/// Delete a property.
///
/// Requires Magisk's patched bionic which provides `__system_property_del`.
/// On stock bionic, this returns `SysPropError::DeleteUnavailable`.
pub fn delete(key: &str, persist: bool) -> SysPropResult<bool> {
    let pi = match find(key) {
        Some(pi) => pi,
        None => return Ok(false),
    };

    delete_internal(pi)?;

    if persist && key.starts_with("persist.") {
        persist::persist_delete_prop(key)?;
    }

    Ok(true)
}

/// Wait for a property to exist or reach a specific value.
///
/// - `expected_value = None`: wait until the property exists.
/// - `expected_value = Some(v)`: wait until the property equals `v`.
/// - `timeout = None`: wait indefinitely.
///
/// Returns `true` if the condition was met, `false` on timeout.
pub fn wait(
    key: &str,
    expected_value: Option<&str>,
    timeout: Option<Duration>,
) -> SysPropResult<bool> {
    let wait_fn = api()
        .wait
        .ok_or(SysPropError::SymbolNotFound("__system_property_wait"))?;

    let deadline = timeout.map(|d| std::time::Instant::now() + d);

    loop {
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                return Ok(false);
            }
        }

        let pi = find(key);

        match (pi, expected_value) {
            (Some(info), Some(expected)) => {
                if let Some(current) = read_value(info) {
                    if current == expected {
                        return Ok(true);
                    }
                }
                // Wait for this property's serial to change.
                let old_serial = serial(info);
                let mut new_serial = 0u32;
                let ts = remaining_timespec(deadline);
                unsafe {
                    wait_fn(info.as_ptr(), old_serial, &mut new_serial, ts_ptr(&ts));
                }
            }
            (Some(_), None) => {
                // Property exists — done.
                return Ok(true);
            }
            (None, _) => {
                // Property doesn't exist — wait for global area serial change.
                let old = area_serial().unwrap_or(0);
                let mut new_serial = 0u32;
                let ts = remaining_timespec(deadline);
                unsafe {
                    wait_fn(
                        std::ptr::null(),
                        old,
                        &mut new_serial,
                        ts_ptr(&ts),
                    );
                }
            }
        }
    }
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn make_cstring(s: &str) -> SysPropResult<CString> {
    CString::new(s).map_err(|_| SysPropError::InvalidCString(s.to_owned()))
}

fn read_value(pi: PropInfoPtr) -> Option<String> {
    struct ValueCookie {
        value: Option<String>,
    }

    unsafe extern "C" fn cb(
        cookie: *mut c_void,
        _name: *const c_char,
        value: *const c_char,
        _serial: u32,
    ) {
        let c = &mut *(cookie as *mut ValueCookie);
        c.value = Some(CStr::from_ptr(value).to_string_lossy().into_owned());
    }

    let mut cookie = ValueCookie { value: None };
    unsafe {
        (api().read_callback)(pi.as_ptr(), Some(cb), &mut cookie as *mut _ as *mut c_void);
    }
    cookie.value
}

fn read_name_value(pi: PropInfoPtr) -> Option<(String, String)> {
    struct NVCookie {
        name: Option<String>,
        value: Option<String>,
    }

    unsafe extern "C" fn cb(
        cookie: *mut c_void,
        name: *const c_char,
        value: *const c_char,
        _serial: u32,
    ) {
        let c = &mut *(cookie as *mut NVCookie);
        c.name = Some(CStr::from_ptr(name).to_string_lossy().into_owned());
        c.value = Some(CStr::from_ptr(value).to_string_lossy().into_owned());
    }

    let mut cookie = NVCookie {
        name: None,
        value: None,
    };
    unsafe {
        (api().read_callback)(pi.as_ptr(), Some(cb), &mut cookie as *mut _ as *mut c_void);
    }
    Some((cookie.name?, cookie.value?))
}

/// Check if a prop_info uses the long-value format.
///
/// Bionic encodes `PROP_INFO_LONG_FLAG (1 << 16)` in the serial field.
/// `__system_property_serial` returns the raw serial which includes this flag.
fn is_long_value(pi: PropInfoPtr) -> bool {
    let s = serial(pi);
    (s & (1 << 16)) != 0
}

fn delete_internal(pi: PropInfoPtr) -> SysPropResult<()> {
    match api().del {
        Some(del_fn) => {
            let ret = unsafe { del_fn(pi.as_ptr()) };
            if ret != 0 {
                return Err(SysPropError::DeleteUnavailable);
            }
            Ok(())
        }
        None => Err(SysPropError::DeleteUnavailable),
    }
}

fn remaining_timespec(deadline: Option<std::time::Instant>) -> Option<libc::timespec> {
    deadline.map(|dl| {
        let remaining = dl.saturating_duration_since(std::time::Instant::now());
        libc::timespec {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_nsec: remaining.subsec_nanos() as libc::c_long,
        }
    })
}

fn ts_ptr(ts: &Option<libc::timespec>) -> *const libc::timespec {
    match ts {
        Some(t) => t as *const _,
        None => std::ptr::null(),
    }
}
