mod prop_area;
mod prop_info;

pub use prop_area::{PropArea, PropAreaError, Result};
pub use prop_info::{
    PropertyInfo, PROP_AREA_HEADER_SIZE, PROP_AREA_MAGIC, PROP_AREA_VERSION, PROP_NAME_MAX,
    PROP_VALUE_MAX,
};
