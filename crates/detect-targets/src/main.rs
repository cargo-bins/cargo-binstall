use std::io;

use detect_targets::detect_targets;
use tokio::runtime;

fn main() -> io::Result<()> {
    #[cfg(feature = "cli-logging")]
    tracing_subscriber::fmt::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::stderr)
        .init();

    let rt = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    if std::env::args().any(|arg| arg == "--probe-all") {
        return probe_all(rt);
    }

    let targets = rt.block_on(detect_targets());

    for target in targets {
        println!("{target}");
    }

    Ok(())
}

/// Run every known loader probe in both dynamic and static mode and
/// print the results, one `target: outcome, static: outcome` line
/// each. The dynamic outcome attests dynamically-linked binaries of
/// that target (arch + libc loader); the static outcome attests
/// statically-linked ones (arch only).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn probe_all(rt: runtime::Runtime) -> io::Result<()> {
    use detect_targets::probe::{probes, ProbeResult};

    fn fmt(result: ProbeResult) -> String {
        match result {
            ProbeResult::Runnable => "runnable".into(),
            ProbeResult::NotRunnable => "not-runnable".into(),
            ProbeResult::Inconclusive(err) => format!("inconclusive: {err}"),
        }
    }

    rt.block_on(async {
        for probe in probes() {
            println!(
                "{}: {}, static: {}",
                probe.target,
                fmt(probe.run().await),
                fmt(probe.run_static().await),
            );
        }
    });

    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn probe_all(_rt: runtime::Runtime) -> io::Result<()> {
    eprintln!("--probe-all is only supported on Linux");
    std::process::exit(1);
}
