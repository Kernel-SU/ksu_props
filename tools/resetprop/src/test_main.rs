use std::process;

use clap::Parser;
use prop_rs_android::sys_prop;

#[derive(Parser)]
#[command(
    name = "resetprop-test",
    version,
    about = "Test-only property inspection helper",
    disable_help_subcommand = true
)]
struct Args {
    /// Print prop serial parts: "<counter> <len>".
    #[arg(long = "serial")]
    serial_parts: bool,

    /// Print the backing prop-area file path for the named property.
    #[arg(long = "area-path")]
    area_path: bool,

    /// Inspect the current storage state of the named property's value slot.
    #[arg(long = "inspect-slot")]
    inspect_slot: bool,

    /// Scan allocations in the named property's backing prop area.
    #[arg(long = "scan")]
    scan: bool,

    /// Property name.
    name: Option<String>,

    /// Unused extra positional, rejected for clarity.
    value: Option<String>,
}

fn main() {
    let args = Args::parse();

    if let Err(e) = sys_prop::init() {
        eprintln!("resetprop-test: {e}");
        process::exit(1);
    }

    if let Err(e) = run(&args) {
        eprintln!("resetprop-test: {e}");
        process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let special_modes = args.serial_parts as u8
        + args.area_path as u8
        + args.inspect_slot as u8
        + args.scan as u8;
    if special_modes != 1 {
        return Err("exactly one inspection mode is required".into());
    }

    let name = args
        .name
        .as_deref()
        .ok_or("an inspection mode requires a property name")?;
    if args.value.is_some() {
        return Err("inspection commands do not accept a property value".into());
    }

    if args.serial_parts {
        let pi = sys_prop::find(name).ok_or_else(|| format!("{name} not found"))?;
        let serial = sys_prop::serial(pi);
        let counter = serial & 0x00ff_ffff;
        let len = serial >> 24;
        println!("{counter} {len}");
        return Ok(());
    }

    if args.area_path {
        println!("{}", sys_prop::area_path(name)?.display());
        return Ok(());
    }

    if args.inspect_slot {
        let info = sys_prop::inspect_value_slot(name)?
            .ok_or_else(|| format!("{name} not found"))?;
        println!("context={}", info.context);
        println!("path={}", info.path.display());
        println!("layout={}", if info.is_long { "long" } else { "inline" });
        println!("value_len={}", info.value_len);
        println!("tail_size={}", info.tail_size);
        println!("tail_nonzero={}", info.tail_nonzero);
        return Ok(());
    }

    let report = sys_prop::scan_area(name)?;
    println!("context={}", report.context);
    println!("path={}", report.path.display());
    println!("bytes_used={}", report.bytes_used);
    println!("has_dirty_backup={}", report.has_dirty_backup);
    println!("objects={}", report.object_count);
    println!("holes={}", report.hole_count);
    for hole in report.holes {
        println!(
            "hole start={} end={} size={} aligned_size={}",
            hole.start_offset, hole.end_offset, hole.size, hole.aligned_size
        );
    }

    Ok(())
}