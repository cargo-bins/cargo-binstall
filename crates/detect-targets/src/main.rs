use std::io;

use detect_targets::detect_targets;
use tokio::runtime;

fn main() -> io::Result<()> {
    #[cfg(feature = "cli-logging")]
    tracing_subscriber::fmt::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::stderr)
        .init();

    let rt = runtime::Builder::new_current_thread().enable_all().build()?;

    if std::env::args().any(|arg| arg == "--probe-all") {
        return probe_all(rt);
    }

    let targets = rt.block_on(detect_targets());

    for target in targets {
        println!("{target}");
    }

    Ok(())
}

/// Run every known loader probe and print its result, one
/// `target: outcome` line each.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn probe_all(rt: runtime::Runtime) -> io::Result<()> {
    use detect_targets::probe::{probes, ProbeResult};

    rt.block_on(async {
        for probe in probes() {
            match probe.run().await {
                ProbeResult::Runnable => println!("{}: runnable", probe.target),
                ProbeResult::NotRunnable => println!("{}: not-runnable", probe.target),
                ProbeResult::Inconclusive(err) => {
                    println!("{}: inconclusive: {err}", probe.target)
                }
            }
        }
    });

    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn probe_all(_rt: runtime::Runtime) -> io::Result<()> {
    eprintln!("--probe-all is only supported on Linux");
    std::process::exit(1);
}
