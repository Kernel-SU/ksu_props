mod prop_area;
mod prop_info;
mod persistent_prop;
pub mod property_context;

pub use prop_area::{
    CompactResult, PropArea, PropAreaAllocationScan, PropAreaError, PropAreaHoleInfo,
    PropAreaObjectInfo, PropAreaObjectKind, Result,
};
pub use prop_info::{
    PropertyInfo, PROP_AREA_HEADER_SIZE, PROP_AREA_MAGIC, PROP_AREA_VERSION, PROP_NAME_MAX,
    PROP_VALUE_MAX,
};
pub use persistent_prop::{
    check_proto, legacy_delete_prop, legacy_get_prop, legacy_list_props, legacy_set_prop,
    PersistentPropError, PersistentProperty, PersistentPropertyFile, PersistentResult,
    ANDROID_PERSISTENT_PROP_DIR, ANDROID_PERSISTENT_PROP_FILE,
};
pub use property_context::{ContextType, PropertyContext};
