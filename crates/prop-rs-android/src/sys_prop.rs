//! Bionic `__system_property_*` API wrapper via `dlsym` + `prop-rs` mmap.
//!
//! This module dynamically loads Android bionic's **standard** system property
//! functions at runtime and exposes a safe Rust API.  For operations that
//! stock bionic does not support (add, update, delete, get_context), we use
//! `prop-rs`'s pure-Rust `PropArea` implementation operating directly on the
//! mmap'd shared-memory files under `/dev/__properties__`.
//!
//! This replaces Magisk's approach of linking against a patched bionic with
//! `__system_property_*2` symbols, making the code work on KernelSU without
//! any bionic modifications.

use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write as IoWrite};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use memmap2::{MmapMut, MmapOptions};
use prop_rs::{
    PropArea, PropAreaError, PropertyContext, AREA_SERIAL_OFFSET, PROP_AREA_HEADER_SIZE,
    PROP_INFO_SERIAL_OFFSET,
};

// ── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SysPropError {
    /// A required bionic symbol was not found via dlsym.
    SymbolNotFound(&'static str),
    /// `__system_properties_init` returned a non-zero error code.
    InitFailed(c_int),
    /// `__system_property_set` returned a non-zero error code.
    SetFailed(c_int),
    /// A prop-area operation failed.
    PropArea(PropAreaError),
    /// An I/O error occurred during mmap operations.
    Io(io::Error),
    /// The property key or value contains an interior NUL byte.
    InvalidCString(String),
    /// A persistent-property I/O operation failed.
    Persistent(prop_rs::PersistentPropError),
}

impl fmt::Display for SysPropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymbolNotFound(sym) => write!(f, "bionic symbol not found: {sym}"),
            Self::InitFailed(code) => write!(f, "__system_properties_init failed: {code}"),
            Self::SetFailed(code) => write!(f, "__system_property_set failed: {code}"),
            Self::PropArea(e) => write!(f, "prop area error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidCString(s) => write!(f, "invalid C string: {s}"),
            Self::Persistent(e) => write!(f, "persistent property error: {e}"),
        }
    }
}

impl std::error::Error for SysPropError {}

impl From<prop_rs::PersistentPropError> for SysPropError {
    fn from(e: prop_rs::PersistentPropError) -> Self {
        Self::Persistent(e)
    }
}

impl From<PropAreaError> for SysPropError {
    fn from(e: PropAreaError) -> Self {
        Self::PropArea(e)
    }
}

impl From<io::Error> for SysPropError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
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
    // Standard bionic symbols (available on all Android versions)
    find: FnFind,
    read_callback: FnReadCallback,
    for_each: FnForEach,
    set: FnSet,
    serial: FnSerial,
    // Optional standard symbols (API 26+)
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

// ── MmapCursor ──────────────────────────────────────────────────────────────

/// A `Read + Write + Seek` cursor over a memory-mapped region.
///
/// This is the same pattern used by `tools/sysprop/src/main.rs` to bridge
/// `memmap2::MmapMut` into `prop_rs::PropArea`.
struct MmapCursor {
    map: MmapMut,
    pos: usize,
}

impl MmapCursor {
    fn new(map: MmapMut) -> Self {
        Self { map, pos: 0 }
    }

    fn flush(&self) -> io::Result<()> {
        self.map.flush()
    }

    /// Return a raw pointer to the start of the mapped region.
    fn as_ptr(&self) -> *const u8 {
        self.map.as_ptr()
    }
}

impl Read for MmapCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.map.as_ref();
        if self.pos >= data.len() {
            return Ok(0);
        }
        let count = (data.len() - self.pos).min(buf.len());
        buf[..count].copy_from_slice(&data[self.pos..self.pos + count]);
        self.pos += count;
        Ok(count)
    }
}

impl Seek for MmapCursor {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = self.map.as_ref().len() as i64;
        let current = self.pos as i64;
        let next = match pos {
            SeekFrom::Start(offset) => i64::try_from(offset)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
            SeekFrom::End(offset) => len
                .checked_add(offset)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
            SeekFrom::Current(offset) => current
                .checked_add(offset)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before start of mmap",
            ));
        }
        self.pos = usize::try_from(next)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?;
        Ok(self.pos as u64)
    }
}

impl IoWrite for MmapCursor {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let data = &mut self.map[..];
        if self.pos > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write past end of mmap",
            ));
        }
        let remaining = data.len() - self.pos;
        if buf.len() > remaining {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "mmap write exceeds mapped region",
            ));
        }
        data[self.pos..self.pos + buf.len()].copy_from_slice(buf);
        self.pos += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.map.flush()
    }
}

// ── PropertyContext singletons ───────────────────────────────────────────────

static PROP_CTX: OnceLock<PropertyContext> = OnceLock::new();

/// Appcompat override context for Android 14+ dual-write support.
/// `None` when the appcompat_override directory does not exist.
static APPCOMPAT_CTX: OnceLock<Option<PropertyContext>> = OnceLock::new();

const APPCOMPAT_DIR: &str = "/dev/__properties__/appcompat_override";

fn prop_ctx() -> SysPropResult<&'static PropertyContext> {
    PROP_CTX
        .get()
        .ok_or_else(|| SysPropError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "PropertyContext not initialized — call sys_prop::init() first",
        )))
}

fn appcompat_ctx() -> Option<&'static PropertyContext> {
    APPCOMPAT_CTX.get().and_then(|opt| opt.as_ref())
}

/// Open a prop area file read-write via shared mmap.
fn open_area_rw(path: &Path) -> SysPropResult<PropArea<MmapCursor>> {
    let f = OpenOptions::new().read(true).write(true).open(path)?;
    let map = unsafe { MmapOptions::new().map_mut(&f) }?;
    Ok(PropArea::new(MmapCursor::new(map))?)
}

// ── Futex helper ────────────────────────────────────────────────────────────

/// Wake all threads waiting on a futex at the given shared-memory address.
///
/// # Safety
///
/// `addr` must point to a valid, shared-memory `u32` (i.e. inside a
/// `MAP_SHARED` mmap region).
unsafe fn futex_wake(addr: *const u32) {
    libc::syscall(
        libc::SYS_futex,
        addr,
        libc::FUTEX_WAKE,
        i32::MAX,
        std::ptr::null::<libc::timespec>(),
    );
}

/// Issue a futex wake for a prop's serial on a mapped prop-area.
///
/// # Safety
///
/// `base` must be the start of a shared-memory prop-area mmap.
unsafe fn wake_prop_serial(base: *const u8, prop_offset: u32) {
    let prop_serial_ptr = base
        .add(PROP_AREA_HEADER_SIZE as usize)
        .add(prop_offset as usize)
        .add(PROP_INFO_SERIAL_OFFSET as usize) as *const u32;
    futex_wake(prop_serial_ptr);
}

/// Bump the global `properties_serial` area serial and issue a futex wake.
///
/// In bionic, this is the serial returned by `__system_property_area_serial()`
/// and waited on by `__system_property_wait(NULL, ...)`.  It lives in a
/// **separate** prop-area file (`properties_serial`), not in the prop-area
/// that contains the property being modified.
fn bump_and_wake_global_serial(ctx: &PropertyContext) -> SysPropResult<()> {
    let serial_path = ctx.serial_prop_area_path();
    let mut serial_area = open_area_rw(&serial_path)?;
    serial_area.bump_area_serial()?;
    let cursor = serial_area.into_inner();
    unsafe {
        let serial_ptr = cursor.as_ptr().add(AREA_SERIAL_OFFSET as usize) as *const u32;
        futex_wake(serial_ptr);
    }
    cursor.flush()?;
    Ok(())
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Load bionic symbols, initialize `PropertyContext`, and call
/// `__system_properties_init`.
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
        serial: load_sym_required("__system_property_serial")?,
        area_serial: load_sym("__system_property_area_serial"),
        wait: load_sym("__system_property_wait"),
    };

    let ret = unsafe { init_fn() };
    if ret != 0 {
        return Err(SysPropError::InitFailed(ret));
    }

    let _ = API.set(bionic);

    // Initialize PropertyContext for prop-rs mmap operations.
    let _ = PROP_CTX.get_or_init(|| {
        PropertyContext::new(
            Path::new("/dev/__properties__"),
            Some(Path::new("/")),
        )
        .expect("failed to load PropertyContext from /dev/__properties__")
    });

    // Initialize appcompat_override context (Android 14+).
    // Silently set to None when the directory does not exist.
    let _ = APPCOMPAT_CTX.get_or_init(|| {
        let dir = PathBuf::from(APPCOMPAT_DIR);
        if !dir.is_dir() {
            return None;
        }
        PropertyContext::new(&dir, Some(Path::new("/"))).ok()
    });

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

/// Get the SELinux context of a property by name.
///
/// Uses `prop-rs`'s `PropertyContext` to resolve the context — no patched
/// bionic required.
pub fn get_context(key: &str) -> SysPropResult<String> {
    let ctx = prop_ctx()?;
    Ok(ctx.get_context_for_name(key).to_string())
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

/// Set a property value.
///
/// - For `ro.*` properties or when `skip_svc` is true: bypasses
///   `property_service` and operates directly on the shared-memory mmap
///   via `prop-rs`'s `PropArea::set_property`.
/// - For other properties: uses `__system_property_set` which goes through
///   init's `property_service` socket.
pub fn set(key: &str, value: &str, skip_svc: bool) -> SysPropResult<()> {
    let force_skip = skip_svc || key.starts_with("ro.");

    if force_skip {
        // Direct mmap write via prop-rs.
        // For non-ro properties we bump the serial so that bionic waiters
        // are notified; for ro properties we keep the serial unchanged to
        // hide the modification.
        let bump = !key.starts_with("ro.");

        let ctx = prop_ctx()?;
        let context = ctx.get_context_for_name(key);
        let area_path = ctx.context_file_path(context);
        let mut area = open_area_rw(&area_path)?;
        let result = area.set_property_with_serial(key, value, bump)?;
        let cursor = area.into_inner();
        if bump {
            unsafe { wake_prop_serial(cursor.as_ptr(), result.prop_offset) };
        }
        cursor.flush()?;

        if bump {
            bump_and_wake_global_serial(ctx)?;
        }

        // Dual-write to appcompat_override area (Android 14+).
        if let Some(appcompat) = appcompat_ctx() {
            let ctx_name = appcompat.get_context_for_name(key);
            let path = appcompat.context_file_path(ctx_name);
            if let Ok(mut area) = open_area_rw(&path) {
                let result = area.set_property_with_serial(key, value, bump);
                let cursor = area.into_inner();
                if bump {
                    if let Ok(r) = &result {
                        unsafe { wake_prop_serial(cursor.as_ptr(), r.prop_offset) };
                    }
                }
                let _ = cursor.flush();
            }

            if bump {
                let _ = bump_and_wake_global_serial(appcompat);
            }
        }
    } else {
        // Go through bionic's property_service socket.
        let ckey = make_cstring(key)?;
        let cval = make_cstring(value)?;
        let ret = unsafe { (api().set)(ckey.as_ptr(), cval.as_ptr()) };
        if ret != 0 {
            return Err(SysPropError::SetFailed(ret));
        }
    }

    Ok(())
}

/// Delete a property from shared memory via `prop-rs`.
///
/// Returns `true` if the property existed and was deleted.
///
/// This does **not** touch persistent storage — the caller is responsible
/// for calling `persist::persist_delete_prop` when appropriate.
pub fn delete(key: &str) -> SysPropResult<bool> {
    let ctx = prop_ctx()?;
    let context = ctx.get_context_for_name(key);
    let area_path = ctx.context_file_path(context);
    let mut area = open_area_rw(&area_path)?;
    let deleted = area.delete_property(key)?;
    let cursor = area.into_inner();
    cursor.flush()?;

    if deleted {
        let _ = bump_and_wake_global_serial(ctx);
    }

    // Dual-delete from appcompat_override area (Android 14+).
    if let Some(appcompat) = appcompat_ctx() {
        let ctx_name = appcompat.get_context_for_name(key);
        let path = appcompat.context_file_path(ctx_name);
        if let Ok(mut area) = open_area_rw(&path) {
            let _ = area.delete_property(key);
            let cursor = area.into_inner();
            let _ = cursor.flush();
        }

        if deleted {
            let _ = bump_and_wake_global_serial(appcompat);
        }
    }

    Ok(deleted)
}

/// Compact all prop area files, reclaiming holes left by deletions.
///
/// Returns `true` if any area was compacted.
pub fn compact() -> SysPropResult<bool> {
    let mut any_compacted = false;

    // Compact main property areas.
    let ctx = prop_ctx()?;
    any_compacted |= compact_areas(ctx)?;

    // Compact appcompat_override areas (Android 14+).
    if let Some(appcompat) = appcompat_ctx() {
        any_compacted |= compact_areas(appcompat)?;
    }

    Ok(any_compacted)
}

fn compact_areas(ctx: &PropertyContext) -> SysPropResult<bool> {
    let targets = ctx.prop_area_files().map_err(SysPropError::Io)?;
    let mut any_compacted = false;

    for (_context, path) in &targets {
        let mut area = match open_area_rw(path) {
            Ok(area) => area,
            Err(_) => continue,
        };
        match area.compact_allocations() {
            Ok(result) => {
                if !matches!(result, prop_rs::CompactResult::NoHoles) {
                    any_compacted = true;
                }
                area.into_inner().flush()?;
            }
            Err(_) => continue,
        }
    }

    Ok(any_compacted)
}

/// Wait for a property to exist or change away from a given value.
///
/// Follows Magisk resetprop semantics:
/// - `old_value = None`: wait until the property exists, then return.
/// - `old_value = Some(v)`: if the current value already differs from `v`,
///   return immediately; otherwise wait until the value changes to something
///   other than `v`.
/// - `timeout = None`: wait indefinitely.
///
/// Returns `true` if the condition was met, `false` on timeout.
pub fn wait(
    key: &str,
    old_value: Option<&str>,
    timeout: Option<Duration>,
) -> SysPropResult<bool> {
    let wait_fn = api()
        .wait
        .ok_or(SysPropError::SymbolNotFound("__system_property_wait"))?;

    let deadline = timeout.map(|d| std::time::Instant::now() + d);

    // Phase 1: wait for the property to exist.
    let info = loop {
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                return Ok(false);
            }
        }

        if let Some(pi) = find(key) {
            break pi;
        }

        // Property doesn't exist — wait for global area serial change.
        let old = area_serial().unwrap_or(0);
        let mut new_serial = 0u32;
        let ts = remaining_timespec(deadline);
        unsafe {
            wait_fn(std::ptr::null(), old, &mut new_serial, ts_ptr(&ts));
        }
    };

    // If no old_value specified, property existence is sufficient.
    let old_value = match old_value {
        Some(v) => v,
        None => return Ok(true),
    };

    // Phase 2: wait for value != old_value.
    loop {
        if let Some(dl) = deadline {
            if std::time::Instant::now() >= dl {
                return Ok(false);
            }
        }

        let mut curr_serial = 0u32;
        if let Some(current) = read_value_serial(info, &mut curr_serial) {
            if current != old_value {
                return Ok(true);
            }
        }

        // Current value still equals old_value — wait for serial change.
        let mut new_serial = 0u32;
        let ts = remaining_timespec(deadline);
        unsafe {
            wait_fn(info.as_ptr(), curr_serial, &mut new_serial, ts_ptr(&ts));
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

/// Read the value and serial of a prop_info in one callback.
fn read_value_serial(pi: PropInfoPtr, out_serial: &mut u32) -> Option<String> {
    struct Cookie {
        value: Option<String>,
        serial: u32,
    }

    unsafe extern "C" fn cb(
        cookie: *mut c_void,
        _name: *const c_char,
        value: *const c_char,
        serial: u32,
    ) {
        let c = &mut *(cookie as *mut Cookie);
        c.value = Some(CStr::from_ptr(value).to_string_lossy().into_owned());
        c.serial = serial;
    }

    let mut cookie = Cookie {
        value: None,
        serial: 0,
    };
    unsafe {
        (api().read_callback)(pi.as_ptr(), Some(cb), &mut cookie as *mut _ as *mut c_void);
    }
    *out_serial = cookie.serial;
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
