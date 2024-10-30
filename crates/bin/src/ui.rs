use std::{
    io::{self, BufRead, StdinLock, Write},
    thread,
};

use binstalk::errors::BinstallError;
use tokio::sync::oneshot;

fn ask_for_confirm(stdin: &mut StdinLock, input: &mut String) -> io::Result<()> {
    {
        let mut stdout = io::stdout().lock();

        write!(&mut stdout, "Do you wish to continue? [yes]/no\n? ")?;
        stdout.flush()?;
    }

    stdin.read_line(input)?;

    Ok(())
}

pub async fn confirm() -> Result<(), BinstallError> {
    let (tx, rx) = oneshot::channel();

    thread::spawn(move || {
        // This task should be the only one able to
        // access stdin
        let mut stdin = io::stdin().lock();
        let mut input = String::with_capacity(16);

        let res = loop {
            if ask_for_confirm(&mut stdin, &mut input).is_err() {
                break false;
            }

            match input.as_str().trim() {
                "" | "yes" | "y" | "YES" | "Y" => break false,
                "no" | "n" | "NO" | "N" => break true,
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
