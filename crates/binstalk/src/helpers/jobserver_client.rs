use std::{num::NonZeroUsize, thread::available_parallelism};

use jobslot::Client;
use tokio::sync::OnceCell;

use crate::errors::BinstallError;

#[derive(Debug)]
pub struct LazyJobserverClient(OnceCell<Client>);

impl LazyJobserverClient {
    /// This must be called at the start of the program since
    /// `Client::from_env` requires that.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // Safety:
        //
        // Client::from_env is unsafe because from_raw_fd is unsafe.
        // It doesn't do anything that is actually unsafe, like
        // dereferencing pointer.
        let opt = unsafe { Client::from_env() };
        Self(OnceCell::new_with(opt))
    }

    pub async fn get(&self) -> Result<&Client, BinstallError> {
        self.0
            .get_or_try_init(|| async {
                let ncore = available_parallelism().map(NonZeroUsize::get).unwrap_or(1);
                Ok(Client::new(ncore)?)
            })
            .await
    }
}
