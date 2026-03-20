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
    name = "cargo-android-resetprop",
    about = "Build Android resetprop, deploy to /data/local/tmp via adb, and optionally run tests via su -c"
)]
struct Args {
    /// Rust target triple (e.g. aarch64-linux-android) or NDK ABI (e.g. arm64-v8a).
    #[arg(long, default_value = "aarch64-linux-android")]
    target: String,

    #[arg(long, value_enum, default_value_t = BuildProfile::Release)]
    profile: BuildProfile,

    /// adb device serial
    #[arg(long)]
    serial: Option<String>,

    /// Remote path to deploy resetprop
    #[arg(long, default_value = "/data/local/tmp/resetprop")]
    remote: String,

    /// adb executable path
    #[arg(long, default_value = "adb")]
    adb: String,

    /// cargo executable path
    #[arg(long, default_value = "cargo")]
    cargo: String,

    /// Run test command automatically after deployment (through su -c).
    #[arg(long)]
    run_test: bool,

    /// Local test script path pushed to device when running tests.
    #[arg(long, default_value = "tests/test_resetprop.sh")]
    test_script: String,

    /// Remote test script path on device.
    #[arg(long, default_value = "/data/local/tmp/test_resetprop.sh")]
    remote_test_script: String,

    /// Command executed by `adb shell su -c '<COMMAND>'`.
    ///
    /// If omitted and tests are enabled, defaults to:
    /// `RESETPROP=<remote> sh <remote_test_script>`
    #[arg(long)]
    test_cmd: Option<String>,
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
        .arg("-p")
        .arg("prop-rs-android")
        .arg("--bin")
        .arg("resetprop")
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
        .join("resetprop");

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

    if args.run_test || args.test_cmd.is_some() {
        let local_test_script = PathBuf::from(&args.test_script);
        if !local_test_script.exists() {
            return Err(format!(
                "test script not found: {}",
                local_test_script.display()
            )
            .into());
        }

        let mut adb_push_test = adb_base(&args);
        adb_push_test
            .arg("push")
            .arg(&local_test_script)
            .arg(&args.remote_test_script)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        run_checked(&mut adb_push_test, "adb push test script")?;

        let mut adb_chmod_test = adb_base(&args);
        adb_chmod_test
            .arg("shell")
            .arg("chmod")
            .arg("+x")
            .arg(&args.remote_test_script)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        run_checked(&mut adb_chmod_test, "adb shell chmod test script")?;

        let cmd = args
            .test_cmd
            .clone()
            .unwrap_or_else(|| format!("RESETPROP={} sh {}", args.remote, args.remote_test_script));

        let mut adb_test = adb_base(&args);
        adb_test
            .arg("shell")
            .arg("su")
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        run_checked(&mut adb_test, "adb shell su -c test")?;
    }

    println!("done: {} -> {}", local_bin.display(), args.remote);
    if let Some(serial) = &args.serial {
        println!("run: adb -s {serial} shell {} --help", args.remote);
    } else {
        println!("run: adb shell {} --help", args.remote);
    }

    Ok(())
}
