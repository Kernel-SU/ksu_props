use std::io::Cursor;

use prop_rs::{
    CompactResult, PropArea, PropAreaObjectKind, PROP_AREA_HEADER_SIZE,
    PROP_AREA_MAGIC, PROP_AREA_VERSION, PROP_VALUE_MAX,
};

fn new_area(size: usize) -> PropArea<Cursor<Vec<u8>>> {
    PropArea::create(Cursor::new(vec![0; size]), size as u64).unwrap()
}

fn data_abs(offset: u32) -> usize {
    PROP_AREA_HEADER_SIZE as usize + offset as usize
}

fn read_serial(raw: &[u8], prop_offset: u32) -> u32 {
    let abs = data_abs(prop_offset);
    u32::from_le_bytes(raw[abs..abs + 4].try_into().unwrap())
}

fn write_u32(raw: &mut [u8], abs: usize, value: u32) {
    raw[abs..abs + 4].copy_from_slice(&value.to_le_bytes());
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

#[test]
fn update_inline_keeps_serial_and_clears_trailing_bytes() {
    let key = "ro.inline.serial";
    let old_value = "ABCDEFGHIJ";
    let new_value = "abc";

    let mut area = new_area(4096);
    area.set_property(key, old_value).unwrap();
    let before = area.get_property_info(key).unwrap().unwrap();
    assert!(!before.is_long);

    let raw_before = area.into_inner().into_inner();
    let serial_before = read_serial(&raw_before, before.prop_offset);

    let mut area = PropArea::new(Cursor::new(raw_before)).unwrap();
    area.set_property(key, new_value).unwrap();
    let after = area.get_property_info(key).unwrap().unwrap();
    assert_eq!(after.prop_offset, before.prop_offset);
    assert_eq!(after.value_offset, before.value_offset);

    let raw_after = area.into_inner().into_inner();
    let serial_after = read_serial(&raw_after, after.prop_offset);
    // High 8 bits encode new value length; low 24 bits preserved from original
    // serial to hide modification traces (Magisk-compatible behaviour).
    let expected_serial = ((new_value.len() as u32) << 24) | (serial_before & 0x00ff_ffff);
    assert_eq!(serial_after, expected_serial);

    let value_abs = data_abs(after.value_offset);
    assert_eq!(&raw_after[value_abs..value_abs + new_value.len()], new_value.as_bytes());
    assert_eq!(raw_after[value_abs + new_value.len()], 0);
    assert!(
        raw_after[value_abs + new_value.len() + 1..value_abs + PROP_VALUE_MAX]
            .iter()
            .all(|&b| b == 0)
    );
}

#[test]
fn update_long_keeps_serial_and_updates_in_place() {
    let key = "persist.sys.long.serial";
    let old_value = "x".repeat(140);
    let new_value = "y".repeat(40);

    let mut area = new_area(16384);
    area.set_property(key, &old_value).unwrap();
    let before = area.get_property_info(key).unwrap().unwrap();
    assert!(before.is_long);

    let raw_before = area.into_inner().into_inner();
    let serial_before = read_serial(&raw_before, before.prop_offset);

    let mut area = PropArea::new(Cursor::new(raw_before)).unwrap();
    area.set_property(key, &new_value).unwrap();
    let after = area.get_property_info(key).unwrap().unwrap();
    assert!(after.is_long);
    assert_eq!(after.value_offset, before.value_offset);

    let raw_after = area.into_inner().into_inner();
    let serial_after = read_serial(&raw_after, after.prop_offset);
    assert_eq!(serial_after, serial_before);

    let value_abs = data_abs(after.value_offset);
    assert_eq!(&raw_after[value_abs..value_abs + new_value.len()], new_value.as_bytes());
    assert_eq!(raw_after[value_abs + new_value.len()], 0);
    assert!(
        raw_after[value_abs + new_value.len() + 1..value_abs + old_value.len() + 1]
            .iter()
            .all(|&b| b == 0)
    );
}

#[test]
fn update_long_auto_delete_add_on_growth() {
    let key = "persist.sys.long.noalloc";
    let old_value = "x".repeat(120);
    let new_value = "y".repeat(121);

    let mut area = new_area(16384);
    area.set_property(key, &old_value).unwrap();

    // Should automatically delete+add when long value grows beyond allocation.
    area.set_property(key, &new_value).unwrap();

    let after = area.get_property_info(key).unwrap().unwrap();
    assert_eq!(after.value, new_value);
    assert!(after.is_long);
}

#[test]
fn update_inline_auto_converts_to_long() {
    let key = "ro.inline.noalloc";
    let old_value = "inline";
    let new_value = "z".repeat(PROP_VALUE_MAX);

    let mut area = new_area(8192);
    area.set_property(key, old_value).unwrap();
    let before = area.get_property_info(key).unwrap().unwrap();
    assert!(!before.is_long);

    // Should automatically delete+add to convert inline to long.
    area.set_property(key, &new_value).unwrap();

    let after = area.get_property_info(key).unwrap().unwrap();
    assert_eq!(after.value, new_value);
    assert!(after.is_long);
}

#[test]
fn scan_allocations_reports_objects_sorted_and_typed() {
    let mut area = new_area(16384);
    area.set_property("ro.scan.inline", "abc").unwrap();
    area.set_property("persist.scan.long", &"x".repeat(120)).unwrap();

    let scan = area.scan_allocations().unwrap();
    assert!(scan.has_dirty_backup);
    assert!(!scan.objects.is_empty());
    assert!(scan
        .objects
        .windows(2)
        .all(|pair| pair[0].offset <= pair[1].offset));

    assert!(scan
        .objects
        .iter()
        .any(|obj| obj.kind == PropAreaObjectKind::DirtyBackup));
    assert!(scan
        .objects
        .iter()
        .any(|obj| obj.kind == PropAreaObjectKind::TrieNode));
    assert!(scan
        .objects
        .iter()
        .any(|obj| obj.kind == PropAreaObjectKind::PropInfo));
    assert!(scan
        .objects
        .iter()
        .any(|obj| obj.kind == PropAreaObjectKind::LongValue));
}

#[test]
fn scan_allocations_reports_hole_after_delete() {
    let key = "ro.scan.hole";
    let mut area = new_area(16384);
    area.set_property(key, "value-to-delete").unwrap();
    let deleted_prop = area.get_property_info(key).unwrap().unwrap();

    assert!(area.delete_property(key).unwrap());

    let scan = area.scan_allocations().unwrap();
    assert!(scan.holes.iter().any(|hole| {
        hole.start_offset <= deleted_prop.prop_offset && deleted_prop.prop_offset < hole.end_offset
    }));
}

#[test]
fn scan_allocations_handles_area_without_dirty_backup() {
    let mut raw = vec![0u8; 1024];

    // Header
    write_u32(&mut raw, 0, 44); // bytes_used
    write_u32(&mut raw, 8, PROP_AREA_MAGIC);
    write_u32(&mut raw, 12, PROP_AREA_VERSION);

    // root node at data offset 0: namelen=0, children=20
    let data0 = PROP_AREA_HEADER_SIZE as usize;
    write_u32(&mut raw, data0 + 0, 0);
    write_u32(&mut raw, data0 + 4, 0);
    write_u32(&mut raw, data0 + 8, 0);
    write_u32(&mut raw, data0 + 12, 0);
    write_u32(&mut raw, data0 + 16, 20);

    // child trie node at data offset 20: name="ro"
    let child = data0 + 20;
    write_u32(&mut raw, child + 0, 2);
    write_u32(&mut raw, child + 4, 0);
    write_u32(&mut raw, child + 8, 0);
    write_u32(&mut raw, child + 12, 0);
    write_u32(&mut raw, child + 16, 0);
    raw[child + 20] = b'r';
    raw[child + 21] = b'o';
    raw[child + 22] = 0;

    let mut area = PropArea::new(Cursor::new(raw)).unwrap();
    let scan = area.scan_allocations().unwrap();

    assert!(!scan.has_dirty_backup);
    assert!(scan
        .objects
        .iter()
        .all(|obj| obj.kind != PropAreaObjectKind::DirtyBackup));
    assert_eq!(scan.holes.len(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// compact_allocations tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn compact_on_area_without_holes_returns_no_holes() {
    let mut area = new_area(4096);
    area.set_property("ro.a", "1").unwrap();
    area.set_property("ro.b", "2").unwrap();

    // No deletions — no holes.
    let result = area.compact_allocations().unwrap();
    assert_eq!(result, CompactResult::NoHoles);

    // Properties are still readable.
    assert_eq!(area.get_property("ro.a").unwrap(), Some("1".to_owned()));
    assert_eq!(area.get_property("ro.b").unwrap(), Some("2".to_owned()));
}

#[test]
fn compact_reclaims_trailing_hole() {
    let mut area = new_area(4096);
    area.set_property("ro.z", "last").unwrap();

    let _bytes_before = area.scan_allocations().unwrap().bytes_used;

    // Delete the sole property — leaves a trailing hole (pruned trie keeps root,
    // but prop_info bytes are zeroed and bytes_used is not yet reduced).
    assert!(area.delete_property("ro.z").unwrap());

    let result = area.compact_allocations().unwrap();
    match result {
        CompactResult::NoHoles => {} // already reclaimed by prune_trie — also acceptable
        CompactResult::AdjustedBytesUsed { old, new } => {
            assert!(new <= old, "bytes_used should not grow");
        }
        CompactResult::MovedObjects { old, new, .. } => {
            assert!(new <= old, "bytes_used should not grow");
        }
    }

    // After compaction the area must be internally consistent.
    let scan = area.scan_allocations().unwrap();
    assert_eq!(scan.holes.len(), 0, "no holes should remain after compact");
}

#[test]
fn compact_moves_objects_over_interior_hole_and_props_still_readable() {
    let mut area = new_area(8192);

    // Insert three properties.  Trie allocates them in insertion order.
    area.set_property("aa", "v1").unwrap();
    area.set_property("bb", "v2").unwrap();
    area.set_property("cc", "v3").unwrap();

    let bytes_before = area.scan_allocations().unwrap().bytes_used;

    // Delete the middle property — creates an interior hole.
    assert!(area.delete_property("bb").unwrap());

    let holes_before = area.scan_allocations().unwrap().holes.len();

    let result = area.compact_allocations().unwrap();

    // After compaction: no holes.
    let scan_after = area.scan_allocations().unwrap();
    assert_eq!(
        scan_after.holes.len(),
        0,
        "compact should eliminate all holes; was: {holes_before} hole(s)"
    );

    // bytes_used must have decreased.
    assert!(
        scan_after.bytes_used <= bytes_before,
        "bytes_used should not exceed pre-delete value"
    );

    // Compact reported a meaningful result.
    match &result {
        CompactResult::NoHoles => {
            // Possible if prune_trie already collapsed everything — still OK.
        }
        CompactResult::AdjustedBytesUsed { old, new } => assert!(new <= old),
        CompactResult::MovedObjects { objects_moved, .. } => {
            assert!(*objects_moved > 0);
        }
    }

    // Surviving properties must still be readable.
    assert_eq!(area.get_property("aa").unwrap(), Some("v1".to_owned()));
    assert_eq!(area.get_property("cc").unwrap(), Some("v3".to_owned()));
    assert_eq!(area.get_property("bb").unwrap(), None);
}

#[test]
fn compact_after_long_property_delete_leaves_no_holes() {
    let mut area = new_area(8192);
    let long_val = "y".repeat(150);

    area.set_property("persist.a", "short").unwrap();
    area.set_property("persist.b", &long_val).unwrap();
    area.set_property("persist.c", "short2").unwrap();

    assert!(area.delete_property("persist.b").unwrap());

    let _result = area.compact_allocations().unwrap();

    let scan = area.scan_allocations().unwrap();
    assert_eq!(scan.holes.len(), 0, "no holes after compacting long-prop deletion");

    assert_eq!(area.get_property("persist.a").unwrap(), Some("short".to_owned()));
    assert_eq!(area.get_property("persist.c").unwrap(), Some("short2".to_owned()));
    assert_eq!(area.get_property("persist.b").unwrap(), None);
}
