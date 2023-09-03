use std::io;

use detect_targets::detect_targets;
use tokio::runtime;

fn main() -> io::Result<()> {
    let targets = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(detect_targets());

    for target in targets {
        println!("{target}");
    }

    Ok(())
}
