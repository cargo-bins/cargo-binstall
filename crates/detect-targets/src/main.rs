use std::io;

use detect_targets::detect_targets;
use tokio::runtime;

fn main() -> io::Result<()> {
    #[cfg(feature = "cli-logging")]
    tracing_subscriber::fmt::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::stderr)
        .init();

    let targets = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(detect_targets());

    for target in targets {
        println!("{target}");
    }

    Ok(())
}
