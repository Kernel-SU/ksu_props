//! `resetprop` — Magisk-compatible system property tool for Android.
//!
//! This binary is a thin CLI wrapper around [`prop_rs_android::resetprop`].
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
//!   resetprop -w name value          Wait until property differs from value
//!
//! Compact:
//!   resetprop -c                     Compact property area memory
//!
//! Flags:
//!   -n          Skip property_service (force direct mmap for all properties)
//!   -p          Also operate on persistent property storage
//!   -P          Only read persistent properties from storage
//!   -c          Compact property area memory
//!   -d          Delete mode
//!   -v          Verbose output
//!   -w          Wait mode
//!   --timeout N Wait timeout in seconds (default: infinite)
//!   -Z          Show SELinux context for each property when listing
//! ```

use std::fs::File;
use std::io::BufRead;
use std::process;
use std::time::Duration;

use clap::Parser;

use prop_rs_android::resetprop::ResetProp;
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

    /// Wait for a property to exist or change.
    #[arg(short = 'w', long = "wait")]
    wait: bool,

    /// Timeout in seconds for --wait (default: wait forever).
    #[arg(long = "timeout")]
    timeout: Option<f64>,

    /// Load and set properties from FILE.
    #[arg(short = 'f', long = "file")]
    file: Option<String>,

    /// Compact property area memory (reclaim holes left by deleted properties).
    #[arg(short = 'c', long = "compact")]
    compact: bool,

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

    if let Err(e) = sys_prop::init() {
        eprintln!("resetprop: {e}");
        process::exit(1);
    }

    let rp = ResetProp {
        skip_svc: args.skip_svc,
        persistent: args.persistent,
        persist_only: args.persist_only,
        verbose: args.verbose,
        show_context: args.show_context,
    };

    if let Err(e) = run(&args, &rp) {
        eprintln!("resetprop: {e}");
        process::exit(1);
    }
}

fn run(args: &Args, rp: &ResetProp) -> Result<(), Box<dyn std::error::Error>> {
    // Validate: at most one special mode.
    let special_modes =
        args.wait as u8 + args.delete as u8 + args.compact as u8 + args.file.is_some() as u8;
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
        let ok = rp.wait(name, args.value.as_deref(), timeout)?;
        if !ok {
            eprintln!("resetprop: timeout waiting for {name}");
            process::exit(2);
        }
        return Ok(());
    }

    // -c: compact property area memory
    if args.compact {
        let compacted = sys_prop::compact()?;
        if !compacted {
            if args.verbose {
                eprintln!("resetprop: nothing to compact");
            }
            process::exit(1);
        }
        return Ok(());
    }

    // -f: load from file
    if let Some(path) = &args.file {
        let file = File::open(path)?;
        let reader = std::io::BufReader::new(file);
        rp.load_props(reader.lines())?;
        return Ok(());
    }

    // -d: delete
    if args.delete {
        let name = args
            .name
            .as_deref()
            .ok_or("--delete requires a property name")?;
        let deleted = rp.delete(name)?;
        if !deleted {
            process::exit(1);
        }
        return Ok(());
    }

    match (&args.name, &args.value) {
        // resetprop name value  (set)
        (Some(name), Some(value)) => {
            rp.set(name, value)?;
        }

        // resetprop name  (get)
        (Some(name), None) => {
            match rp.get(name) {
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
            let props = rp.list_all()?;
            for (name, value) in &props {
                println!("[{name}]: [{value}]");
            }
        }

        // resetprop <no name> <value>  — invalid
        (None, Some(_)) => {
            return Err("property name is required".into());
        }
    }

    Ok(())
}
