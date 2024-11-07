use std::{process::Termination, time::Instant};

use binstalk::{helpers::jobserver_client::LazyJobserverClient, TARGET};
use log::LevelFilter;
use tracing::debug;

use crate::{
    args,
    bin_util::{run_tokio_main, MainExit},
    entry,
    logging::logging,
};

pub fn do_main() -> impl Termination {
    let (args, cli_overrides) = args::parse();

    if args.version {
        let cargo_binstall_version = env!("CARGO_PKG_VERSION");
        if args.verbose {
            let build_date = env!("VERGEN_BUILD_DATE");

            let features = env!("VERGEN_CARGO_FEATURES");

            let git_sha = option_env!("VERGEN_GIT_SHA").unwrap_or("UNKNOWN");
            let git_commit_date = option_env!("VERGEN_GIT_COMMIT_DATE").unwrap_or("UNKNOWN");

            let rustc_semver = env!("VERGEN_RUSTC_SEMVER");
            let rustc_commit_hash = env!("VERGEN_RUSTC_COMMIT_HASH");
            let rustc_llvm_version = env!("VERGEN_RUSTC_LLVM_VERSION");

            println!(
                r#"cargo-binstall: {cargo_binstall_version}
build-date: {build_date}
build-target: {TARGET}
build-features: {features}
build-commit-hash: {git_sha}
build-commit-date: {git_commit_date}
rustc-version: {rustc_semver}
rustc-commit-hash: {rustc_commit_hash}
rustc-llvm-version: {rustc_llvm_version}"#
            );
        } else {
            println!("{cargo_binstall_version}");
        }
        MainExit::Success(None)
    } else if args.self_install {
        MainExit::new(entry::self_install(args), None)
    } else {
        logging(
            args.log_level.unwrap_or(LevelFilter::Info),
            args.json_output,
        );

        let start = Instant::now();

        let jobserver_client = LazyJobserverClient::new();

        let result =
            run_tokio_main(|| entry::install_crates(args, cli_overrides, jobserver_client));

        let done = start.elapsed();
        debug!("run time: {done:?}");

        MainExit::new(result, Some(done))
    }
}
