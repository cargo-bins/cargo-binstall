use log::info;
use std::io::{stderr, stdin, Write};

use crate::BinstallError;

pub fn confirm() -> Result<(), BinstallError> {
    loop {
        info!("Do you wish to continue? yes/[no]");
        eprint!("? ");
        stderr().flush().ok();

        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();

        match input.as_str().trim() {
            "yes" | "y" | "YES" | "Y" => break Ok(()),
            "no" | "n" | "NO" | "N" | "" => break Err(BinstallError::UserAbort),
            _ => continue,
        }
    }
}
