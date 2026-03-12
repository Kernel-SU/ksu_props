pub const PROP_AREA_MAGIC: u32 = 0x504f_5250;
pub const PROP_AREA_VERSION: u32 = 0xfc6e_d0ab;
pub const PROP_AREA_HEADER_SIZE: u64 = 128;
pub(crate) const PROP_TRIE_NODE_HEADER_SIZE: u32 = 20;
pub(crate) const PROP_INFO_SIZE: u32 = 96;
pub const PROP_VALUE_MAX: usize = 92;
pub const PROP_NAME_MAX: usize = 32;
pub(crate) const LONG_LEGACY_ERROR_BUFFER_SIZE: usize = 56;
pub(crate) const LONG_LEGACY_ERROR: &str = "Must use resetprop_property_read_callback() to read";
pub(crate) const LONG_OFFSET_IN_INFO: u32 = 4 + LONG_LEGACY_ERROR_BUFFER_SIZE as u32;
pub(crate) const DIRTY_BACKUP_SIZE: u32 = align_up(PROP_VALUE_MAX as u32, 4);
pub(crate) const INITIAL_BYTES_USED: u32 = PROP_TRIE_NODE_HEADER_SIZE + DIRTY_BACKUP_SIZE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyInfo {
    pub name: String,
    pub value: String,
    pub prop_offset: u32,
    pub name_offset: u32,
    pub value_offset: u32,
    pub is_long: bool,
}

pub(crate) const fn align_up(value: u32, align: u32) -> u32 {
    if align == 0 {
        return value;
    }
    let mask = align - 1;
    (value + mask) & !mask
}
