//! Mmap-backed `prop_area` / `prop_info` accessor that mirrors bionic's
//! C++ `prop_area.cpp` and `prop_info.cpp` exactly, including all atomic
//! memory-ordering requirements for shared-memory concurrent access.
//!
//! # Design
//!
//! `prop-rs`'s `PropArea<M>` uses a generic `Read + Write + Seek` backend and
//! is designed for plain file I/O.  It is **not** safe to use for
//! shared-memory prop areas because:
//!
//! 1. It writes multi-byte fields with plain `copy_from_slice`, which may
//!    be torn under concurrent readers.
//! 2. It uses non-atomic loads/stores for the `serial` and offset fields that
//!    bionic accesses with `atomic_load_explicit` / `atomic_store_explicit`.
//! 3. It rebuilds file-relative offsets differently from the in-memory layout
//!    used by a live Android system.
//!
//! This module works directly with raw pointers into a `MAP_SHARED` mmap
//! region and mirrors the exact memory ordering used by bionic:
//!
//! | Operation         | Bionic ordering  | This module's ordering |
//! |-------------------|------------------|------------------------|
//! | offset ptr load   | `consume`        | `Acquire` (safe approx)|
//! | offset ptr store  | `release`        | `Release`              |
//! | serial read       | `acquire`/`relaxed` | `Acquire`/`Relaxed` |
//! | serial write      | `relaxed`        | `Relaxed`              |
//! | value store fence | `release`        | `Release`              |
//!
//! # Safety
//!
//! All public methods are safe.  The `unsafe` blocks inside implement the
//! raw-pointer arithmetic, atomic operations, and futex syscalls that are
//! inherently unsafe but are guaranteed to be correct given the layout
//! invariants of bionic's prop_area format.

use std::ffi::CStr;
use std::fmt;
use std::sync::atomic::{fence, AtomicU32, Ordering};

use memmap2::MmapMut;
use prop_rs::{
    AREA_SERIAL_OFFSET, PROP_AREA_HEADER_SIZE, PROP_AREA_MAGIC, PROP_AREA_VERSION, PROP_VALUE_MAX,
};

// ── Layout constants (all derived from prop-rs / bionic structs) ─────────────

/// Size of `prop_trie_node` fixed header (5 × u32 = 20 bytes).
const TRIE_HEADER_SIZE: u32 = 20;

/// Size of `prop_info` fixed header (`serial` u32 + `value[92]` = 96 bytes).
const PROP_INFO_SIZE: u32 = 96;

/// Byte offset of `serial` within `prop_info` (= 0).
const PROP_INFO_SERIAL_OFF: u32 = 0;

/// Byte offset of `value` within `prop_info` (= 4).
const PROP_INFO_VALUE_OFF: u32 = 4;

/// Byte offset of `long_property.error_message` (= PROP_INFO_VALUE_OFF = 4).
const PROP_INFO_LONG_ERR_OFF: u32 = PROP_INFO_VALUE_OFF;

/// Byte offset of `long_property.offset` (= 4 + 56 = 60).
const PROP_INFO_LONG_OFFSET_OFF: u32 = PROP_INFO_VALUE_OFF + LONG_LEGACY_ERROR_BUFFER_SIZE as u32;

/// Size of `long_property.error_message[]` (56 bytes).
const LONG_LEGACY_ERROR_BUFFER_SIZE: usize = 56;

/// The inline error message written for long properties.
const LONG_LEGACY_ERROR: &[u8] = b"Must use __system_property_read_callback() to read";

/// `prop_info::kLongFlag` — set in serial when the property uses long-value layout.
pub const PROP_INFO_LONG_FLAG: u32 = 1 << 16;

/// Mask for the counter part of `serial` (low 24 bits).
const SERIAL_COUNTER_MASK: u32 = 0x00ff_ffff;

/// Mask for the length/flags part of `serial` (high 8 bits).
const SERIAL_LEN_MASK: u32 = 0xff00_0000;

/// Byte offset of `prop_trie_node::prop` within the fixed header.
const TRIE_PROP_OFF: u32 = 4;
/// Byte offset of `prop_trie_node::left` within the fixed header.
const TRIE_LEFT_OFF: u32 = 8;
/// Byte offset of `prop_trie_node::right` within the fixed header.
const TRIE_RIGHT_OFF: u32 = 12;
/// Byte offset of `prop_trie_node::children` within the fixed header.
const TRIE_CHILDREN_OFF: u32 = 16;

/// `bytes_used_` in `prop_area` is initialised to sizeof(prop_trie_node) +
/// ALIGN(PROP_VALUE_MAX, 4) to reserve room for the dirty-backup area.


// ── Offsets within prop_area header ─────────────────────────────────────────

/// Byte offset of `bytes_used_` within the `prop_area` header (= 0).
const PA_BYTES_USED_OFF: usize = 0;

/// Byte offset of `serial_` (global area serial) within the `prop_area` header (= 4).
const PA_SERIAL_OFF: usize = AREA_SERIAL_OFFSET as usize;

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum MmapPropAreaError {
    InvalidMagic(u32),
    InvalidVersion(u32),
    InvalidOffset(u32),
    InvalidKey,
    AreaFull,
    ValueTooLong { len: usize },
}

impl fmt::Display for MmapPropAreaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic(m) => write!(f, "invalid prop area magic: 0x{m:08x}"),
            Self::InvalidVersion(v) => write!(f, "invalid prop area version: 0x{v:08x}"),
            Self::InvalidOffset(o) => write!(f, "invalid data offset: {o}"),
            Self::InvalidKey => write!(f, "invalid property key"),
            Self::AreaFull => write!(f, "prop area is full"),
            Self::ValueTooLong { len } => {
                write!(f, "value length {len} >= PROP_VALUE_MAX for mutable property")
            }
        }
    }
}

impl std::error::Error for MmapPropAreaError {}

pub type MmapResult<T> = Result<T, MmapPropAreaError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueSlotInspect {
    pub is_long:      bool,
    pub value_len:    usize,
    pub tail_size:    usize,
    pub tail_nonzero: usize,
}


// ── MmapPropArea ─────────────────────────────────────────────────────────────

/// A live view of a `prop_area` mapped into process memory.
///
/// The underlying `MmapMut` must already be open (the caller is responsible
/// for opening and keeping the mapping alive).
///
/// All pointer arithmetic and atomic operations are confined to the `unsafe`
/// blocks inside this struct's methods.
pub struct MmapPropArea {
    map: MmapMut,
    /// Total size of the mapping in bytes.
    pa_size: usize,
    /// `pa_data_size = pa_size - PROP_AREA_HEADER_SIZE`.
    data_size: u32,
}

impl MmapPropArea {
    /// Open an existing, already-mapped prop area.
    ///
    /// Validates the magic and version fields.
    pub fn new(map: MmapMut) -> MmapResult<Self> {
        let pa_size = map.len();
        let header_size = PROP_AREA_HEADER_SIZE as usize;
        let data_size = (pa_size - header_size) as u32;

        let magic = unsafe { atomic_load_u32_relaxed(map.as_ptr(), 8) };
        let version = unsafe { atomic_load_u32_relaxed(map.as_ptr(), 12) };

        if magic != PROP_AREA_MAGIC {
            return Err(MmapPropAreaError::InvalidMagic(magic));
        }
        if version != PROP_AREA_VERSION {
            return Err(MmapPropAreaError::InvalidVersion(version));
        }

        Ok(Self { map, pa_size, data_size })
    }

    /// Raw pointer to the start of the mmap region (the `prop_area` header).
    #[inline(always)]
    pub fn as_ptr(&self) -> *const u8 {
        self.map.as_ptr()
    }

    /// Raw mutable pointer to the start of the mmap region.
    #[inline(always)]
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.map.as_mut_ptr()
    }

    // ── area-level field accessors ───────────────────────────────────────────

    /// Pointer to the `serial_` field in the `prop_area` header.
    /// Use for futex wake / atomic store.
    pub fn serial_ptr(&self) -> *const u32 {
        unsafe { self.as_ptr().add(PA_SERIAL_OFF) as *const u32 }
    }

    fn read_bytes_used(&self) -> u32 {
        unsafe { atomic_load_u32_relaxed(self.as_ptr(), PA_BYTES_USED_OFF) }
    }

    fn write_bytes_used(&mut self, val: u32) {
        // bytes_used_ is written only by the single writer; Relaxed is fine.
        unsafe { atomic_store_u32_relaxed(self.as_mut_ptr(), PA_BYTES_USED_OFF, val) };
    }

    // ── prop_info serial field accessors ────────────────────────────────────

    /// Absolute byte offset of `prop_info::serial` given the data offset.
    #[inline]
    fn serial_abs_off(&self, data_off: u32) -> usize {
        PROP_AREA_HEADER_SIZE as usize + data_off as usize + PROP_INFO_SERIAL_OFF as usize
    }

    /// Read the serial field of a prop_info atomically with Relaxed ordering.
    unsafe fn read_pi_serial_relaxed(&self, data_off: u32) -> u32 {
        atomic_load_u32_relaxed(self.as_ptr(), self.serial_abs_off(data_off))
    }

    /// Atomically store `serial` into `prop_info::serial` with Relaxed ordering.
    ///
    /// # Safety
    /// `data_off` must be a valid data offset for a `prop_info` record.
    pub unsafe fn store_pi_serial_relaxed(&mut self, data_off: u32, serial: u32) {
        atomic_store_u32_relaxed(
            self.as_mut_ptr(),
            self.serial_abs_off(data_off),
            serial,
        );
    }

    // ── trie pointer accessors (consume/release) ─────────────────────────────

    /// Load a trie node pointer field (prop / left / right / children).
    ///
    /// Bionic uses `memory_order_consume`, which Rust/LLVM conservatively
    /// maps to `Acquire`.
    unsafe fn load_trie_ptr(&self, node_data_off: u32, field_off: u32) -> u32 {
        let abs = PROP_AREA_HEADER_SIZE as usize + node_data_off as usize + field_off as usize;
        let ptr = self.as_ptr().add(abs) as *const AtomicU32;
        (*ptr).load(Ordering::Acquire)
    }

    /// Store a trie node pointer field with `Release` ordering (bionic: `release`).
    unsafe fn store_trie_ptr(&mut self, node_data_off: u32, field_off: u32, val: u32) {
        let abs = PROP_AREA_HEADER_SIZE as usize + node_data_off as usize + field_off as usize;
        let ptr = self.as_mut_ptr().add(abs) as *mut AtomicU32;
        (*ptr).store(val, Ordering::Release);
    }

    // ── raw data helpers ─────────────────────────────────────────────────────

    /// Read a `u32` from a data-space offset with no atomic guarantee.
    /// Safe to use for non-atomic fields (e.g. `namelen`).
    unsafe fn read_u32_data(&self, data_off: u32) -> MmapResult<u32> {
        let abs = PROP_AREA_HEADER_SIZE as usize + data_off as usize;
        if abs + 4 > self.pa_size {
            return Err(MmapPropAreaError::InvalidOffset(data_off));
        }
        let ptr = self.as_ptr().add(abs) as *const u32;
        Ok(ptr.read_unaligned())
    }

    /// Write a `u32` at a data-space offset with no special ordering.
    unsafe fn write_u32_data(&mut self, data_off: u32, val: u32) {
        let abs = PROP_AREA_HEADER_SIZE as usize + data_off as usize;
        let ptr = self.as_mut_ptr().add(abs) as *mut u32;
        ptr.write_unaligned(val);
    }

    /// Copy a byte slice into the mmap at a data-space offset.
    unsafe fn write_bytes_data(&mut self, data_off: u32, src: &[u8]) {
        let abs = PROP_AREA_HEADER_SIZE as usize + data_off as usize;
        let dst = self.as_mut_ptr().add(abs);
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
    }

    /// Read a C-string from data space into a `String`.
    unsafe fn read_cstr_data(&self, data_off: u32, max_len: usize) -> Option<String> {
        let abs = PROP_AREA_HEADER_SIZE as usize + data_off as usize;
        if abs >= self.pa_size {
            return None;
        }
        let slice = std::slice::from_raw_parts(
            self.as_ptr().add(abs),
            (self.pa_size - abs).min(max_len + 1),
        );
        CStr::from_bytes_until_nul(slice)
            .ok()
            .and_then(|c| c.to_str().ok())
            .map(|s| s.to_owned())
    }

    // ── allocator ────────────────────────────────────────────────────────────

    /// Allocate `size` bytes from `data_[]`, aligned to 4 bytes.
    ///
    /// Mirrors `prop_area::allocate_obj`.
    fn allocate(&mut self, size: usize) -> MmapResult<u32> {
        let aligned = (size + 3) & !3;
        let bytes_used = self.read_bytes_used();
        if bytes_used as usize + aligned > self.data_size as usize {
            return Err(MmapPropAreaError::AreaFull);
        }
        self.write_bytes_used(bytes_used + aligned as u32);
        Ok(bytes_used)
    }

    // ── trie node allocation ─────────────────────────────────────────────────

    /// Allocate and initialise a new `prop_trie_node`.
    ///
    /// Mirrors `prop_area::new_prop_trie_node`.
    fn new_trie_node(&mut self, name: &[u8]) -> MmapResult<u32> {
        let total = TRIE_HEADER_SIZE as usize + name.len() + 1;
        let off = self.allocate(total)?;
        unsafe {
            // namelen
            self.write_u32_data(off, name.len() as u32);
            // prop / left / right / children — all zero (already zeroed by mmap on Linux)
            self.write_u32_data(off + TRIE_PROP_OFF, 0);
            self.write_u32_data(off + TRIE_LEFT_OFF, 0);
            self.write_u32_data(off + TRIE_RIGHT_OFF, 0);
            self.write_u32_data(off + TRIE_CHILDREN_OFF, 0);
            // name bytes
            let name_abs = PROP_AREA_HEADER_SIZE as usize + off as usize + TRIE_HEADER_SIZE as usize;
            core::ptr::copy_nonoverlapping(name.as_ptr(), self.as_mut_ptr().add(name_abs), name.len());
            let nul_abs = name_abs + name.len();
            *self.as_mut_ptr().add(nul_abs) = 0;
        }
        Ok(off)
    }

    // ── prop_info allocation ─────────────────────────────────────────────────

    /// Allocate and initialise a new `prop_info` for an inline value.
    ///
    /// The `serial` field is written atomically with `Relaxed` ordering,
    /// matching bionic's `prop_info` constructor:
    ///
    /// ```cpp
    /// atomic_store_explicit(&this->serial, valuelen << 24, memory_order_relaxed);
    /// ```
    ///
    /// Returns the data-space offset of the new `prop_info`.
    fn new_prop_info_inline(
        &mut self,
        name: &[u8],
        value: &[u8],
    ) -> MmapResult<u32> {
        let total = PROP_INFO_SIZE as usize + name.len() + 1;
        let off = self.allocate(total)?;
        unsafe {
            // Write value bytes (not yet visible to readers — serial is still 0).
            let val_abs = off + PROP_INFO_VALUE_OFF;
            self.write_bytes_data(val_abs, value);
            // NUL-terminate value.
            *self.as_mut_ptr()
                .add(PROP_AREA_HEADER_SIZE as usize + val_abs as usize + value.len()) = 0;
            // Write name (after the fixed header).
            let name_abs = off + PROP_INFO_SIZE;
            self.write_bytes_data(name_abs, name);
            *self.as_mut_ptr()
                .add(PROP_AREA_HEADER_SIZE as usize + name_abs as usize + name.len()) = 0;
            // Initialise serial: valuelen << 24, Relaxed.
            let serial = (value.len() as u32) << 24;
            self.store_pi_serial_relaxed(off, serial);
        }
        Ok(off)
    }

    /// Allocate and initialise a new `prop_info` for a long value.
    ///
    /// Mirrors the second `prop_info` constructor:
    /// ```cpp
    /// atomic_store_explicit(&this->serial,
    ///     error_value_len << 24 | kLongFlag, memory_order_relaxed);
    /// ```
    fn new_prop_info_long(
        &mut self,
        name: &[u8],
        value: &[u8],
    ) -> MmapResult<u32> {
        // Allocate prop_info + name.
        let pi_total = PROP_INFO_SIZE as usize + name.len() + 1;
        let pi_off = self.allocate(pi_total)?;

        // Allocate long value storage (value + NUL).
        let lv_off = self.allocate(value.len() + 1)?;

        // `long_property.offset` is relative from `prop_info` start.
        let long_rel_off = lv_off - pi_off;

        unsafe {
            // Write error message into value union.
            let err_abs = pi_off + PROP_INFO_LONG_ERR_OFF;
            let err_bytes = LONG_LEGACY_ERROR.len().min(LONG_LEGACY_ERROR_BUFFER_SIZE - 1);
            self.write_bytes_data(err_abs, &LONG_LEGACY_ERROR[..err_bytes]);
            *self.as_mut_ptr()
                .add(PROP_AREA_HEADER_SIZE as usize + err_abs as usize + err_bytes) = 0;

            // Write long_property.offset.
            let loff_abs = pi_off + PROP_INFO_LONG_OFFSET_OFF;
            self.write_u32_data(loff_abs, long_rel_off);

            // Write long value.
            self.write_bytes_data(lv_off, value);
            *self.as_mut_ptr()
                .add(PROP_AREA_HEADER_SIZE as usize + lv_off as usize + value.len()) = 0;

            // Write name.
            let name_abs = pi_off + PROP_INFO_SIZE;
            self.write_bytes_data(name_abs, name);
            *self.as_mut_ptr()
                .add(PROP_AREA_HEADER_SIZE as usize + name_abs as usize + name.len()) = 0;

            // Initialise serial.
            let error_val_len = LONG_LEGACY_ERROR.len() as u32;
            let serial = (error_val_len << 24) | PROP_INFO_LONG_FLAG;
            self.store_pi_serial_relaxed(pi_off, serial);
        }
        Ok(pi_off)
    }

    // ── trie traversal ───────────────────────────────────────────────────────

    /// Traverse the trie for `name`, optionally allocating missing nodes.
    ///
    /// Returns the data-space offset of the terminal `prop_trie_node` for
    /// `name` on success, or `None` when the node is absent and
    /// `alloc_if_needed` is `false`.
    ///
    /// Mirrors `prop_area::traverse_trie`.
    fn traverse_trie(&mut self, name: &[u8], alloc_if_needed: bool) -> MmapResult<Option<u32>> {
        let mut current = 0u32; // root node at data offset 0

        let mut remaining = name;
        loop {
            let sep = remaining.iter().position(|&b| b == b'.');
            let want_subtree = sep.is_some();
            let substr_len = sep.unwrap_or(remaining.len());
            let substr = &remaining[..substr_len];

            if substr.is_empty() {
                return Err(MmapPropAreaError::InvalidKey);
            }

            // Load or allocate the children list for `current`.
            let children_off = unsafe { self.load_trie_ptr(current, TRIE_CHILDREN_OFF) };
            let root = if children_off != 0 {
                Some(children_off)
            } else if alloc_if_needed {
                let new_off = self.new_trie_node(substr)?;
                unsafe { self.store_trie_ptr(current, TRIE_CHILDREN_OFF, new_off) };
                Some(new_off)
            } else {
                None
            };

            let root = match root {
                Some(r) => r,
                None => return Ok(None),
            };

            let node = self.find_trie_node(root, substr, alloc_if_needed)?;
            current = match node {
                Some(n) => n,
                None => return Ok(None),
            };

            if !want_subtree {
                break;
            }
            remaining = &remaining[substr_len + 1..];
        }

        Ok(Some(current))
    }

    /// Find (or allocate) a node in the binary-search tree rooted at `trie_off`
    /// whose `name` matches `key`.
    ///
    /// Mirrors `prop_area::find_prop_trie_node`.
    fn find_trie_node(
        &mut self,
        trie_off: u32,
        key: &[u8],
        alloc_if_needed: bool,
    ) -> MmapResult<Option<u32>> {
        let mut current = trie_off;
        loop {
            let namelen = unsafe { self.read_u32_data(current)? };

            let name_abs = PROP_AREA_HEADER_SIZE as usize
                + current as usize
                + TRIE_HEADER_SIZE as usize;
            let cur_name = unsafe {
                std::slice::from_raw_parts(self.as_ptr().add(name_abs), namelen as usize)
            };

            use std::cmp::Ordering;
            let ord = cmp_name(key, cur_name);
            match ord {
                Ordering::Equal => return Ok(Some(current)),
                Ordering::Less => {
                    let left = unsafe { self.load_trie_ptr(current, TRIE_LEFT_OFF) };
                    if left != 0 {
                        current = left;
                    } else if alloc_if_needed {
                        let new_off = self.new_trie_node(key)?;
                        unsafe { self.store_trie_ptr(current, TRIE_LEFT_OFF, new_off) };
                        return Ok(Some(new_off));
                    } else {
                        return Ok(None);
                    }
                }
                Ordering::Greater => {
                    let right = unsafe { self.load_trie_ptr(current, TRIE_RIGHT_OFF) };
                    if right != 0 {
                        current = right;
                    } else if alloc_if_needed {
                        let new_off = self.new_trie_node(key)?;
                        unsafe { self.store_trie_ptr(current, TRIE_RIGHT_OFF, new_off) };
                        return Ok(Some(new_off));
                    } else {
                        return Ok(None);
                    }
                }
            }
        }
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Find a property by name, returning its data-space offset.
    ///
    /// Returns `None` when the property does not exist.
    ///
    /// Takes `&mut self` because the traversal uses atomic loads on node
    /// pointer fields (which require mutable access in Rust's memory model
    /// even though no writes occur in the non-allocating path).
    pub fn find(&mut self, name: &str) -> MmapResult<Option<u32>> {
        match self.traverse_trie(name.as_bytes(), false)? {
            None => Ok(None),
            Some(node_off) => {
                let prop_off = unsafe { self.load_trie_ptr(node_off, TRIE_PROP_OFF) };
                Ok(if prop_off != 0 { Some(prop_off) } else { None })
            }
        }
    }

    /// Read the name and value of a `prop_info` at data-space `data_off`.
    ///
    /// Returns `None` when the offset is invalid.
    pub fn read_prop(&self, data_off: u32) -> Option<(String, String)> {
        unsafe {
            let serial = self.read_pi_serial_relaxed(data_off);
            let is_long = (serial & PROP_INFO_LONG_FLAG) != 0;
            let name_off = data_off + PROP_INFO_SIZE;

            let name = self.read_cstr_data(name_off, 256)?;

            let value = if is_long {
                let loff = self.read_u32_data(data_off + PROP_INFO_LONG_OFFSET_OFF).ok()?;
                // loff is relative from prop_info start
                self.read_cstr_data(data_off + loff, 1024)?
            } else {
                let val_len = (serial >> 24) as usize;
                let val_abs = (PROP_AREA_HEADER_SIZE as usize)
                    + data_off as usize
                    + PROP_INFO_VALUE_OFF as usize;
                let slice = std::slice::from_raw_parts(
                    self.as_ptr().add(val_abs),
                    val_len.min(PROP_VALUE_MAX),
                );
                String::from_utf8_lossy(slice).into_owned()
            };

            Some((name, value))
        }
    }

    /// Read the serial of a `prop_info` at data-space `data_off`.
    pub fn read_serial(&self, data_off: u32) -> u32 {
        unsafe { self.read_pi_serial_relaxed(data_off) }
    }

    /// Inspect the current value-slot state for a property.
    ///
    /// For inline properties, `tail_*` describes `value[len..PROP_VALUE_MAX]`.
    /// For long properties, the inline slot is not relevant, so `tail_*` is zero.
    pub fn inspect_value_slot(&mut self, name: &str) -> MmapResult<Option<ValueSlotInspect>> {
        let Some(data_off) = self.find(name)? else {
            return Ok(None);
        };

        let serial = unsafe { self.read_pi_serial_relaxed(data_off) };
        let is_long = (serial & PROP_INFO_LONG_FLAG) != 0;

        if is_long {
            let loff = unsafe { self.read_u32_data(data_off + PROP_INFO_LONG_OFFSET_OFF)? };
            let lv_abs = PROP_AREA_HEADER_SIZE as usize + (data_off + loff) as usize;
            let value_len = unsafe {
                libc::strlen(self.as_ptr().add(lv_abs) as *const libc::c_char)
            };
            return Ok(Some(ValueSlotInspect {
                is_long: true,
                value_len,
                tail_size: 0,
                tail_nonzero: 0,
            }));
        }

        let value_len = ((serial >> 24) as usize).min(PROP_VALUE_MAX);
        let val_abs = PROP_AREA_HEADER_SIZE as usize
            + data_off as usize
            + PROP_INFO_VALUE_OFF as usize;
        let tail = unsafe {
            std::slice::from_raw_parts(
                self.as_ptr().add(val_abs + value_len),
                PROP_VALUE_MAX - value_len,
            )
        };

        Ok(Some(ValueSlotInspect {
            is_long: false,
            value_len,
            tail_size: tail.len(),
            tail_nonzero: tail.iter().filter(|&&b| b != 0).count(),
        }))
    }

    /// Add a new property and publish serial changes using bionic's writer protocol.
    ///
    /// If the property already exists its value is updated via [`Self::update`].
    pub fn add(
        &mut self,
        name: &str,
        value: &str,
        serial_pa: &mut MmapPropArea,
    ) -> MmapResult<()> {
        let name_b = name.as_bytes();
        let value_b = value.as_bytes();

        let node_off = match self.traverse_trie(name_b, true)? {
            Some(o) => o,
            None => return Err(MmapPropAreaError::InvalidKey),
        };

        // Property already exists — delegate to update.
        let existing_off = unsafe { self.load_trie_ptr(node_off, TRIE_PROP_OFF) };
        if existing_off != 0 {
            return self.update(existing_off, value, serial_pa);
        }

        let is_long = value_b.len() >= PROP_VALUE_MAX;
        let pi_off = if is_long {
            self.new_prop_info_long(name_b, value_b)?
        } else {
            self.new_prop_info_inline(name_b, value_b)?
        };

        // Publish the prop_info pointer into the trie node (Release).
        unsafe { self.store_trie_ptr(node_off, TRIE_PROP_OFF, pi_off) };

        serial_pa.bump_area_serial_and_wake();
        Ok(())
    }

    /// Update the value of an existing `prop_info` at data-space `data_off`.
    ///
    /// All validation runs **before** the dirty bit is set — if validation
    /// fails the prop serial is left untouched.
    pub fn update(
        &mut self,
        data_off: u32,
        value: &str,
        serial_pa: &mut MmapPropArea,
    ) -> MmapResult<()> {
        let value_b = value.as_bytes();
        let old_serial = unsafe { self.read_pi_serial_relaxed(data_off) };
        let is_long = (old_serial & PROP_INFO_LONG_FLAG) != 0;

        // ── Validate before touching serial ─────────────────────────────────
        let (lv_data_off, max_long_len) = if is_long {
            let loff = unsafe { self.read_u32_data(data_off + PROP_INFO_LONG_OFFSET_OFF)? };
            let lv_data_off = data_off + loff;
            let lv_abs = PROP_AREA_HEADER_SIZE as usize + lv_data_off as usize;
            let old_long_len = unsafe {
                libc::strlen(self.as_ptr().add(lv_abs) as *const libc::c_char)
            };
            let max_long_len = ((old_long_len + 1 + 3) & !3).saturating_sub(1);
            if value_b.len() > max_long_len {
                return Err(MmapPropAreaError::ValueTooLong { len: value_b.len() });
            }
            (lv_data_off, max_long_len)
        } else {
            if value_b.len() >= PROP_VALUE_MAX {
                return Err(MmapPropAreaError::ValueTooLong { len: value_b.len() });
            }
            (0, 0)
        };

        // ── Set dirty bit ────────────────────────────────────────────────────
        let serial_dirty = old_serial | 1;
        unsafe { self.store_pi_serial_relaxed(data_off, serial_dirty) };

        // ── Write new value ──────────────────────────────────────────────────
        if is_long {
            unsafe {
                self.write_bytes_data(lv_data_off, value_b);
                *self.as_mut_ptr()
                    .add(PROP_AREA_HEADER_SIZE as usize + lv_data_off as usize + value_b.len()) = 0;
                if value_b.len() < max_long_len {
                    let tail_abs = PROP_AREA_HEADER_SIZE as usize
                        + lv_data_off as usize
                        + value_b.len() + 1;
                    let tail_len = max_long_len - (value_b.len() + 1);
                    core::ptr::write_bytes(self.as_mut_ptr().add(tail_abs), 0, tail_len);
                }
            }
        } else {
            let val_abs = data_off + PROP_INFO_VALUE_OFF;
            unsafe {
                self.write_bytes_data(val_abs, value_b);
                let tail_abs = PROP_AREA_HEADER_SIZE as usize
                    + val_abs as usize
                    + value_b.len();
                let tail_len = PROP_VALUE_MAX - value_b.len();
                core::ptr::write_bytes(self.as_mut_ptr().add(tail_abs), 0, tail_len);
            }
        }

        // ── Publish serials (bionic Update() protocol) ───────────────────────
        let serial_abs_off = self.serial_abs_off(data_off);
        let serial_len = if is_long {
            LONG_LEGACY_ERROR.len() as u32
        } else {
            value_b.len() as u32
        };

        release_fence();
        let visible = compose_visible_serial(serial_dirty, serial_len, is_long);
        unsafe {
            let ptr = self.as_ptr().add(serial_abs_off) as *mut AtomicU32;
            (*ptr).store(visible, Ordering::Relaxed);
            futex_wake(self.as_ptr().add(serial_abs_off) as *const u32);
        }

        serial_pa.bump_area_serial_and_wake();

        release_fence();
        let hidden = compose_hidden_serial(serial_dirty, serial_len, is_long);
        unsafe {
            let ptr = self.as_ptr().add(serial_abs_off) as *mut AtomicU32;
            (*ptr).store(hidden, Ordering::Relaxed);
        }

        Ok(())
    }

    /// DFS prune pass used after a deletion.
    ///
    /// Returns `true` when `node_off` became a redundant leaf and should be
    /// detached from its parent, matching bionic's `prune_trie` behavior.
    fn prune_trie(&mut self, node_off: u32) -> MmapResult<bool> {
        let mut is_leaf = true;

        let children = unsafe { self.load_trie_ptr(node_off, TRIE_CHILDREN_OFF) };
        if children != 0 {
            if self.prune_trie(children)? {
                unsafe { self.store_trie_ptr(node_off, TRIE_CHILDREN_OFF, 0) };
            } else {
                is_leaf = false;
            }
        }

        let left = unsafe { self.load_trie_ptr(node_off, TRIE_LEFT_OFF) };
        if left != 0 {
            if self.prune_trie(left)? {
                unsafe { self.store_trie_ptr(node_off, TRIE_LEFT_OFF, 0) };
            } else {
                is_leaf = false;
            }
        }

        let right = unsafe { self.load_trie_ptr(node_off, TRIE_RIGHT_OFF) };
        if right != 0 {
            if self.prune_trie(right)? {
                unsafe { self.store_trie_ptr(node_off, TRIE_RIGHT_OFF, 0) };
            } else {
                is_leaf = false;
            }
        }

        let prop = unsafe { self.load_trie_ptr(node_off, TRIE_PROP_OFF) };
        if is_leaf && prop == 0 {
            let namelen = unsafe { self.read_u32_data(node_off)? } as usize;
            let node_abs = PROP_AREA_HEADER_SIZE as usize + node_off as usize;
            let name_abs = node_abs + TRIE_HEADER_SIZE as usize;
            unsafe {
                if name_abs + namelen <= self.pa_size {
                    core::ptr::write_bytes(self.as_mut_ptr().add(name_abs), 0, namelen);
                }
                if node_abs + TRIE_HEADER_SIZE as usize <= self.pa_size {
                    core::ptr::write_bytes(self.as_mut_ptr().add(node_abs), 0, TRIE_HEADER_SIZE as usize);
                }
            }
            return Ok(true);
        }

        Ok(false)
    }

    /// Remove a property from the trie.
    ///
    /// The node's `prop` pointer is zeroed (Release), the `prop_info` memory
    /// is wiped, and then a prune pass removes redundant leaf trie nodes.
    /// Returns `true` when the property was found and removed.
    pub fn remove(&mut self, name: &str) -> MmapResult<bool> {
        self.remove_with_prune(name, true)
    }

    /// Remove a property from the trie with configurable prune behavior.
    pub fn remove_with_prune(&mut self, name: &str, prune: bool) -> MmapResult<bool> {
        let node_off = match self.traverse_trie(name.as_bytes(), false)? {
            Some(o) => o,
            None => return Ok(false),
        };
        let prop_off = unsafe { self.load_trie_ptr(node_off, TRIE_PROP_OFF) };
        if prop_off == 0 {
            return Ok(false);
        }

        let serial = unsafe { self.read_pi_serial_relaxed(prop_off) };
        let is_long = (serial & PROP_INFO_LONG_FLAG) != 0;

        // Detach from trie ASAP (Release store of 0).
        unsafe { self.store_trie_ptr(node_off, TRIE_PROP_OFF, 0) };

        // Wipe the prop_info record.
        unsafe {
            if is_long {
                let loff = self.read_u32_data(prop_off + PROP_INFO_LONG_OFFSET_OFF)?;
                let lv_abs =
                    PROP_AREA_HEADER_SIZE as usize + (prop_off + loff) as usize;
                let lv_len = {
                    let lv = self.as_ptr().add(lv_abs);
                    libc::strlen(lv as *const libc::c_char)
                };
                core::ptr::write_bytes(self.as_mut_ptr().add(lv_abs), 0, lv_len);
            }
            // Wipe the name field (starts at prop_off + PROP_INFO_SIZE).
            let name_abs = PROP_AREA_HEADER_SIZE as usize
                + prop_off as usize
                + PROP_INFO_SIZE as usize;
            let name_len = {
                let np = self.as_ptr().add(name_abs);
                libc::strlen(np as *const libc::c_char)
            };
            core::ptr::write_bytes(self.as_mut_ptr().add(name_abs), 0, name_len);
            // Wipe the fixed header (PROP_INFO_SIZE bytes).
            let pi_abs = PROP_AREA_HEADER_SIZE as usize + prop_off as usize;
            core::ptr::write_bytes(self.as_mut_ptr().add(pi_abs), 0, PROP_INFO_SIZE as usize);
        }

        if prune {
            let _ = self.prune_trie(0);
        }

        Ok(true)
    }

    /// Absolute byte offset of the area's `serial_` field (= 4).
    pub fn area_serial_abs_off(&self) -> usize {
        PA_SERIAL_OFF
    }

    /// Bump the global area serial with Release ordering and issue a futex
    /// wake, matching bionic's Add() / Delete() / Update() pattern.
    ///
    /// # Safety
    ///
    /// Must only be called by the single writer.
    pub fn bump_area_serial_and_wake(&mut self) {
        unsafe {
            let old = atomic_load_u32_relaxed(self.as_ptr(), PA_SERIAL_OFF);
            atomic_store_u32_release(self.as_mut_ptr(), PA_SERIAL_OFF, old.wrapping_add(1));
            futex_wake(self.as_ptr().add(PA_SERIAL_OFF) as *const u32);
        }
    }

    /// Add or update a property and publish serial changes using bionic's
    /// writer protocol. `serial_pa` must be the global properties serial area.
    pub fn upsert(
        &mut self,
        name: &str,
        value: &str,
        serial_pa: &mut MmapPropArea,
    ) -> MmapResult<()> {
        if let Some(data_off) = self.find(name)? {
            self.update(data_off, value, serial_pa)
        } else {
            self.add(name, value, serial_pa)
        }
    }
}

// ── Serial composition helpers ────────────────────────────────────────────────

/// Compose the initial serial for a newly created `prop_info`.
///
/// Matches bionic `prop_info` constructor: `valuelen << 24` (Relaxed).
pub fn compose_initial_serial(serial_len: u32, is_long: bool) -> u32 {
    let mut s = (serial_len << 24) & SERIAL_LEN_MASK;
    if is_long {
        s |= PROP_INFO_LONG_FLAG;
    }
    s
}

/// Compose the first "visible" serial written during `Update()`.
///
/// `serial_dirty = old_serial | 1`.  The visible serial is:
/// `len_flags | ((serial_dirty + 1) & 0x00ff_ffff)`.
pub fn compose_visible_serial(serial_dirty: u32, serial_len: u32, is_long: bool) -> u32 {
    let mut s = (serial_len << 24) & SERIAL_LEN_MASK;
    if is_long {
        s |= PROP_INFO_LONG_FLAG;
    }
    s | (serial_dirty.wrapping_add(1) & SERIAL_COUNTER_MASK)
}

/// Compose the final "hidden" serial written at the end of `Update()` to
/// restore the counter so that it doesn't reveal that a modification was
/// made.
///
/// `new_serial = len_flags | ((serial_dirty & ~1) & 0x00ff_ffff)`.
pub fn compose_hidden_serial(serial_dirty: u32, serial_len: u32, is_long: bool) -> u32 {
    let mut s = (serial_len << 24) & SERIAL_LEN_MASK;
    if is_long {
        s |= PROP_INFO_LONG_FLAG;
    }
    s | ((serial_dirty & !1u32) & SERIAL_COUNTER_MASK)
}

// ── Low-level atomic / futex utilities ───────────────────────────────────────

/// Atomically load a `u32` at `base + abs_off` with Relaxed ordering.
///
/// # Safety
///
/// `base + abs_off` must be 4-byte aligned and within a valid mapping.
#[inline]
unsafe fn atomic_load_u32_relaxed(base: *const u8, abs_off: usize) -> u32 {
    let ptr = base.add(abs_off) as *const AtomicU32;
    (*ptr).load(Ordering::Relaxed)
}

/// Atomically store `val` at `base + abs_off` with Relaxed ordering.
///
/// # Safety
///
/// `base + abs_off` must be 4-byte aligned and within a valid mapping.
#[inline]
unsafe fn atomic_store_u32_relaxed(base: *mut u8, abs_off: usize, val: u32) {
    let ptr = base.add(abs_off) as *mut AtomicU32;
    (*ptr).store(val, Ordering::Relaxed);
}

/// Atomically store `val` at `base + abs_off` with Release ordering.
///
/// # Safety
///
/// `base + abs_off` must be 4-byte aligned and within a valid mapping.
#[inline]
unsafe fn atomic_store_u32_release(base: *mut u8, abs_off: usize, val: u32) {
    let ptr = base.add(abs_off) as *mut AtomicU32;
    (*ptr).store(val, Ordering::Release);
}

/// Issue a futex wake for all threads waiting on `addr`.
///
/// # Safety
///
/// `addr` must point to a valid `u32` within a `MAP_SHARED` region.
pub unsafe fn futex_wake(addr: *const u32) {
    libc::syscall(
        libc::SYS_futex,
        addr,
        libc::FUTEX_WAKE,
        i32::MAX,
        std::ptr::null::<libc::timespec>(),
    );
}

/// Issue a futex wake on a `prop_info::serial` field.
///
/// # Safety
///
/// `base` must be the start of the mmap region, `data_off` must be a valid
/// `prop_info` data-space offset.
pub unsafe fn futex_wake_pi_serial(base: *const u8, data_off: u32) {
    let addr = base
        .add(PROP_AREA_HEADER_SIZE as usize)
        .add(data_off as usize)
        .add(PROP_INFO_SERIAL_OFF as usize) as *const u32;
    futex_wake(addr);
}

/// A `Release` fence matching bionic's `atomic_thread_fence(memory_order_release)`.
#[inline]
pub fn release_fence() {
    fence(Ordering::Release);
}

// ── Name comparison ───────────────────────────────────────────────────────────

/// Compare two property name segments, mirroring `cmp_prop_name`.
fn cmp_name(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}
