use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

/// Run `cmd` and return its trimmed stdout, or `None` if it can't be
/// spawned or exits unsuccessfully.
fn output(cmd: &mut Command) -> Option<String> {
    let output = cmd.stderr(Stdio::null()).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

/// Converts days since 1970-01-01 to a (year, month, day) in the proleptic
/// Gregorian calendar.
///
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

fn emit_build_date() {
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let secs = match env::var("SOURCE_DATE_EPOCH") {
        Ok(epoch) => epoch
            .parse()
            .expect("SOURCE_DATE_EPOCH must be a unix timestamp"),
        Err(_) => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    let (year, month, day) = civil_from_days(secs / 86400);
    println!("cargo:rustc-env=BUILD_INFO_DATE={year:04}-{month:02}-{day:02}");
}

fn emit_cargo_features() {
    let mut features: Vec<_> = env::vars_os()
        .filter_map(|(key, _)| Some(key.to_str()?.strip_prefix("CARGO_FEATURE_")?.to_lowercase()))
        .collect();
    features.sort_unstable();
    println!("cargo:rustc-env=BUILD_INFO_FEATURES={}", features.join(","));
}

fn emit_rustc_info() {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = output(Command::new(rustc).arg("-vV")).expect("failed to run `rustc -vV`");
    let field = |name: &str| {
        version
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
            .unwrap_or("unknown")
    };

    println!(
        "cargo:rustc-env=BUILD_INFO_RUSTC_SEMVER={}",
        field("release")
    );
    println!(
        "cargo:rustc-env=BUILD_INFO_RUSTC_COMMIT_HASH={}",
        field("commit-hash")
    );

    // Only the major and minor version, e.g. `21.1`.
    let llvm_version = field("LLVM version");
    let llvm_version = match llvm_version.match_indices('.').nth(1) {
        Some((i, _)) => &llvm_version[..i],
        None => llvm_version,
    };
    println!("cargo:rustc-env=BUILD_INFO_RUSTC_LLVM_VERSION={llvm_version}");
}

fn git(args: &[&str]) -> Option<String> {
    output(Command::new("git").args(args))
}

/// Emits the commit sha and date when building from a git checkout. A
/// published crate has neither, and the binary prints `UNKNOWN` instead.
fn emit_git_info() {
    // .git is usually a dir, but it also can be a file containing
    // path to another .git if it is a submodule.
    //
    // If build.rs is run on a git repository, then ../../.git
    // should exists.
    if !Path::new("../../.git").exists() {
        return;
    }
    let Some(sha) = git(&["rev-parse", "HEAD"]) else {
        return;
    };
    println!("cargo:rustc-env=BUILD_INFO_GIT_SHA={sha}");
    if let Some(date) = git(&["log", "-1", "--pretty=format:%cs"]) {
        println!("cargo:rustc-env=BUILD_INFO_GIT_COMMIT_DATE={date}");
    }

    // Rerun when HEAD moves to another branch or commit.
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]).map(PathBuf::from) {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        if let Some(head_ref) = git(&["symbolic-ref", "HEAD"]) {
            let ref_path = git_dir.join(head_ref);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
        }
    }
}

const RERUN_INSTRUCTIONS: &str = "cargo:rerun-if-changed=build.rs
cargo:rerun-if-changed=manifest.rc
cargo:rerun-if-changed=windows.manifest";

fn main() {
    thread::scope(|s| {
        let handle = s.spawn(|| {
            println!("{RERUN_INSTRUCTIONS}");

            embed_resource::compile("manifest.rc", embed_resource::NONE)
                .manifest_required()
                .unwrap();
        });

        emit_build_date();
        emit_cargo_features();
        emit_rustc_info();
        emit_git_info();

        handle.join().unwrap();
    });
}
