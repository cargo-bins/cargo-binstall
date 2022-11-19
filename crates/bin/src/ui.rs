use std::{
    io::{self, BufRead, Write},
    thread,
};

use binstalk::errors::BinstallError;
use tokio::sync::oneshot;

pub async fn confirm() -> Result<(), BinstallError> {
    let (tx, rx) = oneshot::channel();

    thread::spawn(move || {
        // This task should be the only one able to
        // access stdin
        let mut stdin = io::stdin().lock();
        let mut input = String::with_capacity(16);

        let res = loop {
            {
                let mut stdout = io::stdout().lock();

                writeln!(&mut stdout, "Do you wish to continue? yes/[no]\n? ").unwrap();
                stdout.flush().unwrap();
            }

            stdin.read_line(&mut input).unwrap();

            match input.as_str().trim() {
                "yes" | "y" | "YES" | "Y" => break true,
                "no" | "n" | "NO" | "N" | "" => break false,
                _ => {
                    input.clear();
                    continue;
                }
            }
        };

        // The main thread might be terminated by signal and thus cancelled
        // the confirmation.
        tx.send(res).ok();
    });

    if rx.await.unwrap() {
        Ok(())
    } else {
        Err(BinstallError::UserAbort)
    }
}
