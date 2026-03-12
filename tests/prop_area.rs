use std::io::Cursor;

use resetprop_rs::PropArea;

fn new_area(size: usize) -> PropArea<Cursor<Vec<u8>>> {
    PropArea::create(Cursor::new(vec![0; size]), size as u64).unwrap()
}

#[test]
fn add_get_update_and_delete_short_property() {
    let mut area = new_area(4096);

    assert_eq!(area.get_property("ro.secure").unwrap(), None);

    area.set_property("ro.secure", "1").unwrap();
    assert_eq!(area.get_property("ro.secure").unwrap(), Some("1".to_owned()));

    area.set_property("ro.secure", "0").unwrap();
    let info = area.get_property_info("ro.secure").unwrap().unwrap();
    assert_eq!(info.name, "ro.secure");
    assert_eq!(info.value, "0");
    assert!(!info.is_long);

    assert!(area.delete_property("ro.secure").unwrap());
    assert_eq!(area.get_property("ro.secure").unwrap(), None);
}

#[test]
fn long_property_round_trip_and_update() {
    let mut area = new_area(8192);
    let long_value = "x".repeat(140);

    area.set_property("persist.sys.long", &long_value).unwrap();

    let info = area.get_property_info("persist.sys.long").unwrap().unwrap();
    assert_eq!(info.value, long_value);
    assert!(info.is_long);

    area.set_property("persist.sys.long", "short").unwrap();
    let updated = area.get_property_info("persist.sys.long").unwrap().unwrap();
    assert_eq!(updated.value, "short");
    assert!(updated.is_long);
}

#[test]
fn foreach_reports_all_properties() {
    let mut area = new_area(8192);
    area.set_property("ro.secure", "1").unwrap();
    area.set_property("persist.sys.locale", "en-US").unwrap();
    area.set_property("persist.sys.timezone", "UTC").unwrap();

    let mut props = Vec::new();
    area.for_each_property(|info| props.push((info.name, info.value)))
        .unwrap();

    props.sort();
    assert_eq!(
        props,
        vec![
            ("persist.sys.locale".to_owned(), "en-US".to_owned()),
            ("persist.sys.timezone".to_owned(), "UTC".to_owned()),
            ("ro.secure".to_owned(), "1".to_owned()),
        ]
    );
}

#[test]
fn delete_prunes_only_removed_branch() {
    let mut area = new_area(8192);
    area.set_property("persist.sys.locale", "en-US").unwrap();
    area.set_property("persist.sys.timezone", "UTC").unwrap();

    assert!(area.delete_property("persist.sys.locale").unwrap());
    assert_eq!(area.get_property("persist.sys.locale").unwrap(), None);
    assert_eq!(
        area.get_property("persist.sys.timezone").unwrap(),
        Some("UTC".to_owned())
    );
}
