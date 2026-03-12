use std::error::Error;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BuildProfile {
    Debug,
    Release,
}

impl BuildProfile {
    fn as_dir(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "cargo_android_sysprop",
    about = "Build Android sysprop via cargo ndk and deploy it to /data/local/tmp via adb"
)]
struct Args {
    /// Rust target triple (e.g. aarch64-linux-android) or NDK ABI
    /// (e.g. arm64-v8a).
    #[arg(long, default_value = "aarch64-linux-android")]
    target: String,

    #[arg(long, value_enum, default_value_t = BuildProfile::Release)]
    profile: BuildProfile,

    #[arg(long)]
    serial: Option<String>,

    #[arg(long, default_value = "/data/local/tmp/sysprop")]
    remote: String,

    #[arg(long, default_value = "adb")]
    adb: String,

    #[arg(long, default_value = "cargo")]
    cargo: String,
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn resolve_target(input: &str) -> Result<(String, String)> {
    let (rust_target, ndk_abi) = match input {
        "aarch64-linux-android" => ("aarch64-linux-android", "arm64-v8a"),
        "armv7-linux-androideabi" => ("armv7-linux-androideabi", "armeabi-v7a"),
        "i686-linux-android" => ("i686-linux-android", "x86"),
        "x86_64-linux-android" => ("x86_64-linux-android", "x86_64"),

        "arm64-v8a" => ("aarch64-linux-android", "arm64-v8a"),
        "armeabi-v7a" => ("armv7-linux-androideabi", "armeabi-v7a"),
        "x86" => ("i686-linux-android", "x86"),
        "x86_64" => ("x86_64-linux-android", "x86_64"),

        _ => {
            return Err(format!(
                "unsupported --target '{input}'. supported rust targets: \
aarch64-linux-android, armv7-linux-androideabi, i686-linux-android, x86_64-linux-android; \
or NDK ABI: arm64-v8a, armeabi-v7a, x86, x86_64"
            )
            .into())
        }
    };
    Ok((rust_target.to_string(), ndk_abi.to_string()))
}

fn run_checked(cmd: &mut Command, step: &str) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("{step} failed with status: {status}").into());
    }
    Ok(())
}

fn adb_base(args: &Args) -> Command {
    let mut cmd = Command::new(&args.adb);
    if let Some(serial) = &args.serial {
        cmd.arg("-s").arg(serial);
    }
    cmd
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let (rust_target, ndk_abi) = resolve_target(&args.target)?;

    let mut cargo_build = Command::new(&args.cargo);
    cargo_build
        .arg("ndk")
        .arg("-t")
        .arg(&ndk_abi)
        .arg("build")
        .arg("--bin")
        .arg("sysprop")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if matches!(args.profile, BuildProfile::Release) {
        cargo_build.arg("--release");
    }

    run_checked(&mut cargo_build, "cargo ndk build")?;

    let local_bin = PathBuf::from("target")
        .join(&rust_target)
        .join(args.profile.as_dir())
        .join("sysprop");

    if !local_bin.exists() {
        return Err(format!("built binary not found: {}", local_bin.display()).into());
    }

    let mut adb_push = adb_base(&args);
    adb_push
        .arg("push")
        .arg(&local_bin)
        .arg(&args.remote)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    run_checked(&mut adb_push, "adb push")?;

    let mut adb_chmod = adb_base(&args);
    adb_chmod
        .arg("shell")
        .arg("chmod")
        .arg("+x")
        .arg(&args.remote)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    run_checked(&mut adb_chmod, "adb shell chmod")?;

    println!("done: {} -> {}", local_bin.display(), args.remote);
    if let Some(serial) = &args.serial {
        println!("run: adb -s {serial} shell {} --help", args.remote);
    } else {
        println!("run: adb shell {} --help", args.remote);
    }

    Ok(())
}
