use std::{
    io::{self, BufRead, StdinLock, Write},
    thread,
};

use binstalk::errors::BinstallError;
use tokio::sync::oneshot;

fn ask_for_confirm(stdin: &mut StdinLock, input: &mut String, prompt: &str) -> io::Result<()> {
    {
        let mut stdout = io::stdout().lock();

        write!(&mut stdout, "{prompt}",)?;
        stdout.flush()?;
    }

    stdin.read_line(input)?;

    Ok(())
}

pub async fn confirm() -> Result<(), BinstallError> {
    let (tx, rx) = oneshot::channel();

    thread::spawn(move || {
        let res = confirm_sync("Do you wish to continue? [yes]/no ", true);

        // The main thread might be terminated by signal and thus cancel the confirmation
        tx.send(res).ok();
    });

    if rx.await.unwrap() {
        Ok(())
    } else {
        Err(BinstallError::UserAbort)
    }
}

pub fn confirm_sync(prompt: &str, default: bool) -> bool {
    // This task should be the only one able to access stdin
    let mut stdin = io::stdin().lock();
    let mut input = String::with_capacity(16);

    loop {
        if ask_for_confirm(&mut stdin, &mut input, prompt).is_err() {
            break false;
        }

        match input.as_str().trim() {
            "" => break default,
            "yes" | "y" | "YES" | "Y" => break true,
            "no" | "n" | "NO" | "N" => break false,
            _ => {
                input.clear();
                continue;
            }
        }
    }
}
