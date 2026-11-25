use std::{
    io::{self, BufRead, StdinLock, Write},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
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

/// How long the user has spent answering prompts, so that it can be
/// excluded from the reported run time.
///
/// Clones share the same total.
#[derive(Clone, Debug, Default)]
pub struct TimeInPrompts(Arc<Mutex<Duration>>);

impl TimeInPrompts {
    pub fn get(&self) -> Duration {
        *self.0.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn add(&self, duration: Duration) {
        *self.0.lock().unwrap_or_else(|err| err.into_inner()) += duration;
    }
}

pub async fn confirm(time_in_prompts: &TimeInPrompts) -> Result<(), BinstallError> {
    let (tx, rx) = oneshot::channel();
    let time_in_prompts = time_in_prompts.clone();

    thread::spawn(move || {
        let res = confirm_sync("Do you wish to continue? [yes]/no ", true, &time_in_prompts);

        // The main thread might be terminated by signal and thus cancel the confirmation
        tx.send(res).ok();
    });

    if rx.await.unwrap() {
        Ok(())
    } else {
        Err(BinstallError::UserAbort)
    }
}

pub fn confirm_sync(prompt: &str, default: bool, time_in_prompts: &TimeInPrompts) -> bool {
    let start = Instant::now();
    let res = confirm_sync_inner(prompt, default);
    time_in_prompts.add(start.elapsed());
    res
}

fn confirm_sync_inner(prompt: &str, default: bool) -> bool {
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
