# resetprop-rs

[简体中文](README.zh-CN.md)

`resetprop-rs` is a Rust toolkit for Android system property storage.

It can:

- parse raw Android `prop_area` files
- read, write, update, and delete properties
- resolve property names to SELinux contexts through Android `property_contexts`
- inspect allocation layout, holes, and dirty-backup regions
- compact holes left behind by deletions
- read and write Android persistent property files (`persistent_properties` protobuf format)
- run as both a reusable library and a practical CLI

Despite the name, this is **not** a wrapper around Magisk's `resetprop`. It directly understands Android's property-area data structures and context metadata.

## Why this project exists

Android's native property implementation is powerful, but it is not very convenient when the goal is:

- offline inspection of copied `/dev/__properties__` data
- host-side debugging on Windows/Linux/macOS
- writing tests against real or synthetic property areas
- building small custom tools without pulling in Android-specific runtime dependencies

`resetprop-rs` keeps compatibility with the underlying layout while exposing the behavior as a focused Rust crate and CLI.

## Advantages

### 1. Safer implementation language

The project is written in Rust rather than C/C++. That does not magically remove every bug, but it does reduce entire classes of memory-safety problems that are common in binary parsers and in-place editors.

For a tool that reads and mutates low-level binary structures, that is a practical advantage.

### 2. Works off-device

The property-context parser uses plain file I/O and can operate on copied Android filesystem data.

That means the project is useful even when the device is not rooted, not connected, or not available at all. It fits well into reverse-engineering workflows, ROM bring-up, CI, regression testing, and forensic/offline analysis.

### 3. Better observability than typical property tools

Most property tools focus on `get` and `set`.

This project also exposes the internal allocation state:

- `scan` shows live objects, holes, and dirty-backup presence
- allocation objects are typed (`trie-node`, `prop-info`, `long-value`, `dirty-backup`)
- `compact` can reclaim space after deletions

That makes it easier to debug fragmentation, validate assumptions about the on-disk layout, and inspect how updates actually affect the property area.

### 4. Practical mutation support

The crate does not stop at parsing:

- update inline values in place
- update long values in place when possible
- delete properties
- compact freed holes after deletion

This is useful for fixture generation, controlled experiments, and offline modification of copied property-area files.

### 5. Context-aware routing

The main `sysprop` CLI can route a property name to the correct prop-area file by reading Android property-context data.

Supported context storage modes:

- **Serialized**
- **Split**
- **PreSplit**

This mirrors the major Android property-context layouts and avoids hard-coding a single storage model.

### 6. Designed for low-privilege and host-side inspection

When enumerating prop-area files, the project prefers known context metadata over directory enumeration where possible. That is helpful on Android systems where low-privilege users may not be allowed to list `/dev/__properties__` directly.

### 7. Useful as both a library and a tool

The repository includes:

- `prop-rs`: a reusable, platform-independent Rust library
- `prop-rs-android`: Android platform bindings (bionic system property API, SELinux)
- `sysprop`: the main context-aware CLI for offline prop-area analysis
- `resetprop`: Android-specific CLI counterpart to Magisk's resetprop
- `read_props`: a minimal raw prop-area reader
- `write_props`: a minimal raw prop-area writer
- `cargo-android-sysprop`: helper for building and pushing to Android via `cargo ndk` + `adb`

### 8. Tested against synthetic and fixture-based cases

The test suite covers:

- short and long properties
- read/write/delete behavior
- in-place update rules
- allocation scanning
- dirty-backup detection
- compaction after deletion
- fixture-based reads/writes against a sample prop-area file

## Project layout

```
ksu_props/
├── crates/
│   ├── prop-rs/                  # core library (platform-independent)
│   │   ├── src/
│   │   │   ├── lib.rs            — public library exports
│   │   │   ├── prop_area.rs      — low-level prop-area parsing, editing, scanning, compaction
│   │   │   ├── prop_info.rs      — property info types and constants
│   │   │   ├── property_context.rs — Android property-context parsing and context resolution
│   │   │   └── persistent_prop.rs  — persistent property protobuf CRUD (pure Rust, no libc)
│   │   └── tests/                — integration tests and fixtures
│   └── prop-rs-android/          # Android platform bindings (bionic dlsym, SELinux)
│       └── src/
│           ├── lib.rs
│           ├── sys_prop.rs       — bionic __system_property_* API wrapper
│           ├── resetprop.rs      — core resetprop operations (platform-independent business logic)
│           └── persist.rs        — unified persistent property API with SELinux label preservation
├── tools/
│   ├── sysprop/                  # platform-independent CLI (offline prop-area analysis)
│   │   └── src/
│   │       ├── main.rs           — main CLI with context-routed operations
│   │       ├── read_props.rs     — simple raw reader
│   │       └── write_props.rs    — simple raw writer
│   ├── resetprop/                # Android-specific CLI (counterpart to Magisk's resetprop)
│   ├── gen-sample-props/         # test fixture generator
│   └── cargo-android-sysprop/    # build & deploy helper (cargo ndk + adb)
└── Cargo.toml
```

## Build

```bash
cargo build
cargo test
```

Build only the main CLI:

```bash
cargo build --bin sysprop
```

## Main CLI: `sysprop`

Show help:

```bash
cargo run --bin sysprop -- --help
```

### Context-routed operations

These commands resolve the correct prop-area file from Android property-context metadata:

```bash
cargo run --bin sysprop -- --props-dir <PROPS_DIR> get ro.build.fingerprint
cargo run --bin sysprop -- --props-dir <PROPS_DIR> set persist.sys.locale en-US
cargo run --bin sysprop -- --props-dir <PROPS_DIR> del persist.sys.locale
cargo run --bin sysprop -- --props-dir <PROPS_DIR> list --show-context
cargo run --bin sysprop -- --props-dir <PROPS_DIR> scan --objects
cargo run --bin sysprop -- --props-dir <PROPS_DIR> compact
```

#### `--persistent` flag (Android only)

`set` and `del` support a `--persistent` flag that writes to **both** the prop area and
`/data/property/persistent_properties`, so the change survives a reboot:

```bash
# Write to prop area AND persist across reboots
sysprop --props-dir <PROPS_DIR> set persist.sys.locale en-US --persistent
# Delete from prop area AND remove from persistent storage
sysprop --props-dir <PROPS_DIR> del persist.sys.locale --persistent
```

`get` and `list` also accept `--persistent`; in that mode they read directly from
`/data/property/persistent_properties` instead of the prop area.

If you only want one context:

```bash
cargo run --bin sysprop -- --props-dir <PROPS_DIR> scan --context u:object_r:build_prop:s0 --objects
cargo run --bin sysprop -- --props-dir <PROPS_DIR> compact --context u:object_r:build_prop:s0
```

On non-Android hosts, `--system-root <ANDROID_ROOT>` may also be needed when the context storage format is `Split`.

### Persistent property file operations

The `persistent-file` subcommand operates on an Android `persistent_properties` protobuf file
directly. It works on any platform; on non-Android hosts `--path` is required.

```bash
# List all persistent properties
cargo run --bin sysprop -- persistent-file --path /data/property/persistent_properties list

# Get a single property
cargo run --bin sysprop -- persistent-file --path /data/property/persistent_properties get persist.sys.locale

# Set a property (atomic write, survives reboot)
cargo run --bin sysprop -- persistent-file --path /data/property/persistent_properties set persist.sys.locale en-US

# Delete a property
cargo run --bin sysprop -- persistent-file --path /data/property/persistent_properties del persist.sys.locale
```

On Android the path defaults to `/data/property/persistent_properties` and may be omitted:

```bash
sysprop persistent-file list
sysprop persistent-file get persist.sys.locale
```

### Single-area operations

These commands target one specific prop-area file directly:

```bash
cargo run --bin sysprop -- area --path tests/fixtures/sample_props.prop list
cargo run --bin sysprop -- area --path tests/fixtures/sample_props.prop scan --objects
cargo run --bin sysprop -- area --path tests/fixtures/sample_props.prop compact
```

You can also select an area by context name:

```bash
cargo run --bin sysprop -- --props-dir <PROPS_DIR> area --context u:object_r:build_prop:s0 scan --objects
```

## Android CLI: `resetprop`

A Magisk-compatible command-line tool for manipulating Android system properties at runtime.
Unlike the main `sysprop` CLI (which is designed for offline analysis), `resetprop` operates on a
live Android device using bionic's `__system_property_*` API combined with direct mmap writes via `prop-rs`.

```bash
# List all properties
resetprop

# Get a property value
resetprop ro.build.fingerprint

# Set a property (goes through property_service)
resetprop persist.sys.locale en-US

# Set a property bypassing property_service (direct mmap)
resetprop -n ro.debuggable 1

# Delete a property
resetprop -d ro.test.prop

# Also operate on persistent storage (survives reboot)
resetprop -p -n persist.sys.locale en-US
resetprop -p -d persist.sys.locale

# Read only from persistent storage
resetprop -P persist.sys.locale

# Wait for a property to exist
resetprop -w sys.boot_completed

# Wait until a property differs from a value (with timeout)
resetprop -w sys.boot_completed 0 --timeout 30

# Load properties from a file
resetprop -f /path/to/props.txt

# Compact property area memory (reclaim holes from deletions)
resetprop -c

# Show SELinux contexts
resetprop -Z
```

### Flags

| Flag | Description |
|------|-------------|
| `-n` | Skip `property_service`, force direct mmap write |
| `-p` | Also operate on persistent property storage (`persist.*`) |
| `-P` | Only read from persistent storage |
| `-d` | Delete mode |
| `-w` | Wait mode |
| `-c` | Compact property area memory |
| `-v` | Verbose output |
| `-Z` | Show SELinux context instead of value |
| `-f FILE` | Load and set properties from file |
| `--timeout N` | Wait timeout in seconds (default: infinite) |

## Minimal tools

### Read a raw prop-area file

```bash
cargo run --bin read_props -- tests/fixtures/sample_props.prop
cargo run --bin read_props -- tests/fixtures/sample_props.prop ro.product.locale
```

### Write a raw prop-area file

```bash
cargo run --bin write_props -- tests/fixtures/sample_props.prop ro.product.locale=en-US
```

## Deploy `sysprop` to Android

If `cargo ndk` and `adb` are available:

```bash
cargo run --bin cargo-android-sysprop -- --target aarch64-linux-android --profile release
```

The helper builds `sysprop`, pushes it to the device, and marks it executable.

## Library usage

```rust
use std::fs::File;
use resetprop_rs::PropArea;

let file = File::open("tests/fixtures/sample_props.prop")?;
let mut area = PropArea::new(file)?;

if let Some(info) = area.get_property_info("ro.product.locale")? {
    println!("{} = {}", info.name, info.value);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Scope and non-goals

This project focuses on understanding and manipulating Android property-area data.

It is **not** a full replacement for Android's property service, and it does not try to emulate every runtime behavior of init, SELinux policy enforcement, or the complete Android property stack.

## Current status

The project is already useful for inspection, experimentation, and tooling, especially when the main goal is visibility and control over raw property-area data.

If the problem is "understand what is in this property area and change it safely enough for offline or controlled workflows", this repository is aimed directly at that use case.
