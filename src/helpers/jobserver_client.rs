use std::num::NonZeroUsize;
use std::thread::available_parallelism;

use crate::BinstallError;

pub fn create_jobserver_client() -> Result<jobserver::Client, BinstallError> {
    use jobserver::Client;

    // Safety:
    //
    // Client::from_env is unsafe because from_raw_fd is unsafe.
    // It doesn't do anything that is actually unsafe, like
    // dereferencing pointer.
    if let Some(client) = unsafe { Client::from_env() } {
        Ok(client)
    } else {
        let ncore = available_parallelism().map(NonZeroUsize::get).unwrap_or(1);
        Ok(Client::new(ncore)?)
    }
}
