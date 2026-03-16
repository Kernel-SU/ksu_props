//! `resetprop` — Magisk-compatible system property tool for Android.
//!
//! # Usage
//!
//! ```text
//! resetprop [flags] [name [value]]
//!
//! Read:
//!   resetprop                        List all properties
//!   resetprop name                   Get property value
//!
//! Write:
//!   resetprop name value             Set property (non-ro.* via property_service)
//!   resetprop -n name value          Skip property_service, direct mmap
//!   -f, --file FILE                  Load and set properties from FILE
//!
//! Delete:
//!   resetprop -d name                Delete property
//!
//! Wait:
//!   resetprop -w name                Wait for property to exist
//!   resetprop -w name value          Wait for property to equal value
//!
//! Flags:
//!   -n          Skip property_service (force direct mmap for all properties)
//!   -p          Also operate on persistent property storage
//!   -P          Only read persistent properties from storage
//!   -d          Delete mode
//!   -v          Verbose output
//!   -w          Wait mode
//!   --timeout N Wait timeout in seconds (default: infinite)
//!   -Z          Show SELinux context for each property when listing
//! ```

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process;
use std::time::Duration;

use clap::Parser;

use prop_rs_android::persist;
use prop_rs_android::sys_prop;

/// Magisk-compatible Android system property tool.
#[derive(Parser)]
#[command(
    name = "resetprop",
    version,
    about = "Magisk-compatible system property tool",
    disable_help_subcommand = true
)]
struct Args {
    /// Skip property_service (force direct mmap operation).
    #[arg(short = 'n', long = "skip-svc")]
    skip_svc: bool,

    /// Also operate on persistent property storage (persist.* files).
    #[arg(short = 'p', long = "persistent")]
    persistent: bool,

    /// Only read persistent properties from storage.
    #[arg(short = 'P')]
    persist_only: bool,

    /// Delete the named property.
    #[arg(short = 'd', long = "delete")]
    delete: bool,

    /// Verbose output.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Wait for a property to exist or match a value.
    #[arg(short = 'w', long = "wait")]
    wait: bool,

    /// Timeout in seconds for --wait (default: wait forever).
    #[arg(long = "timeout")]
    timeout: Option<f64>,

    /// Load and set properties from FILE.
    #[arg(short = 'f', long = "file")]
    file: Option<String>,

    /// Show SELinux context when listing properties.
    #[arg(short = 'Z')]
    show_context: bool,

    /// Property name.
    name: Option<String>,

    /// Property value (for set or wait-for-value).
    value: Option<String>,
}

fn main() {
    let args = Args::parse();

    if let Err(e) = run(&args) {
        eprintln!("resetprop: {e}");
        process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    sys_prop::init()?;

    // Validate: at most one special mode
    let special_modes =
        args.wait as u8 + args.delete as u8 + args.file.is_some() as u8;
    if special_modes > 1 {
        return Err("multiple operation modes detected".into());
    }

    // -w: wait mode
    if args.wait {
        let name = args
            .name
            .as_deref()
            .ok_or("--wait requires a property name")?;
        let timeout = args.timeout.map(Duration::from_secs_f64);
        let ok = sys_prop::wait(name, args.value.as_deref(), timeout)?;
        if !ok {
            eprintln!("resetprop: timeout waiting for {name}");
            process::exit(2);
        }
        return Ok(());
    }

    // -f: load from file
    if let Some(path) = &args.file {
        load_file(args, path)?;
        return Ok(());
    }

    // -d: delete
    if args.delete {
        let name = args
            .name
            .as_deref()
            .ok_or("--delete requires a property name")?;
        let deleted = sys_prop::delete(name, args.persistent)?;
        if args.verbose {
            if deleted {
                eprintln!("resetprop: deleted {name}");
            } else {
                eprintln!("resetprop: {name} not found");
            }
        }
        if !deleted {
            process::exit(1);
        }
        return Ok(());
    }

    match (&args.name, &args.value) {
        // resetprop name value  (set)
        (Some(name), Some(value)) => {
            sys_prop::set(name, value, args.skip_svc)?;
            if args.persistent && name.starts_with("persist.") {
                // sys_prop::set already handles persist for skip_svc/ro.*,
                // but if the user explicitly asks -p on a non-skip path,
                // we persist here too.
                if !args.skip_svc && !name.starts_with("ro.") {
                    persist::persist_set_prop(name, value)?;
                }
            }
            if args.verbose {
                eprintln!("resetprop: set {name}={value}");
            }
        }

        // resetprop name  (get)
        (Some(name), None) => {
            let val = get_prop(args, name);
            match val {
                Some(val) => println!("{val}"),
                None => {
                    if args.verbose {
                        eprintln!("resetprop: {name} not found");
                    }
                    process::exit(1);
                }
            }
        }

        // resetprop  (list all)
        (None, None) => {
            print_all(args)?;
        }

        // resetprop <no name> <value>  — invalid
        (None, Some(_)) => {
            return Err("property name is required".into());
        }
    }

    Ok(())
}

/// Get a property value, respecting -p/-P flags.
fn get_prop(args: &Args, name: &str) -> Option<String> {
    let mut val = if !args.persist_only {
        sys_prop::get(name)
    } else {
        None
    };

    if val.is_none() && (args.persistent || args.persist_only) && name.starts_with("persist.") {
        val = persist::persist_get_prop(name).ok().flatten();
    }

    val
}

/// Print all properties, respecting -p/-P/-Z flags.
fn print_all(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut props: Vec<(String, String)> = Vec::new();

    if !args.persist_only {
        sys_prop::for_each(|name, value| {
            props.push((name.to_owned(), value.to_owned()));
        });
    }

    if args.persistent || args.persist_only {
        let persist_props = persist::persist_get_all_props()?;
        for (name, value) in persist_props {
            // Persistent props merge: only add if not already present from sys
            if !props.iter().any(|(n, _)| n == &name) {
                props.push((name, value));
            }
        }
    }

    props.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in &props {
        println!("[{name}]: [{value}]");
    }
    Ok(())
}

/// Load properties from a file (one `key=value` or `key value` per line).
///
/// Mirrors Magisk's `load_file` / `BufReadExt::for_each_prop` behavior:
/// - Lines starting with `#` are comments
/// - Empty lines are skipped
/// - Key and value are separated by `=` (with optional surrounding whitespace)
fn load_file(args: &Args, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split on '='
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };

        if key.is_empty() {
            continue;
        }

        sys_prop::set(key, value, args.skip_svc)?;

        if args.persistent && key.starts_with("persist.") {
            if args.skip_svc || key.starts_with("ro.") {
                // Already handled by sys_prop::set for skip_svc
            } else {
                persist::persist_set_prop(key, value)?;
            }
        }

        if args.verbose {
            eprintln!("resetprop: set {key}={value}");
        }
    }
    Ok(())
}
