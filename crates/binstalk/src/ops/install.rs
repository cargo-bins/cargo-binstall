use std::sync::Arc;

use tokio::task::block_in_place;
use tracing::instrument;

use super::{resolve::Resolution, Options};
use crate::{errors::BinstallError, manifests::crate_info::CrateInfo};

#[instrument(skip_all)]
pub async fn install(
    resolution: Resolution,
    opts: Arc<Options>,
) -> Result<Option<CrateInfo>, BinstallError> {
    match resolution {
        Resolution::AlreadyUpToDate => Ok(None),
        Resolution::Fetch(resolution_fetch) => block_in_place(|| resolution_fetch.install(&opts)),
        Resolution::InstallFromSource(resolution_install_from_source) => {
            resolution_install_from_source
                .install(opts)
                .await
                .map(|()| None)
        }
    }
}
