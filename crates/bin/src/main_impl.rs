use std::{process::Termination, time::Instant};

use binstalk::{TARGET, helpers::jobserver_client::LazyJobserverClient};
use log::LevelFilter;
use tracing::debug;

use crate::{
    args,
    bin_util::{MainExit, run_tokio_main},
    entry,
    logging::logging,
    ui::TimeInPrompts,
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
    } else if args.list {
        MainExit::new(entry::list_crates(args), None)
    } else if args.prune {
        MainExit::new(entry::prune_crates(args), None)
    } else {
        logging(
            args.log_level.unwrap_or(LevelFilter::Info),
            args.json_output,
        );

        let start = Instant::now();

        let jobserver_client = LazyJobserverClient::new();

        let time_in_prompts = TimeInPrompts::default();

        let result = run_tokio_main(|| {
            entry::install_crates(
                args,
                cli_overrides,
                jobserver_client,
                time_in_prompts.clone(),
            )
        });

        let elapsed = start.elapsed();
        let in_prompts = time_in_prompts.get();
        debug!("run time: {elapsed:?}, waiting for user input: {in_prompts:?}");

        // Don't count the time the user took to answer the prompts.
        let done = elapsed.saturating_sub(in_prompts);

        MainExit::new(result, Some(done))
    }
}
