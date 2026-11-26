use crate::{
    Detected,
    compat::{Host, Incompatible},
    detect,
};

use std::{path::Path, sync::Arc};

use tokio::sync::SetOnce;

#[derive(Debug)]
enum DesiredTargetsInner {
    AutoDetect(Arc<SetOnce<Detected>>),
    Initialized(Vec<String>),
}

#[derive(Debug)]
pub struct DesiredTargets(DesiredTargetsInner);

impl DesiredTargets {
    fn initialized(targets: Vec<String>) -> Self {
        Self(DesiredTargetsInner::Initialized(targets))
    }

    fn auto_detect() -> Self {
        let arc = Arc::new(SetOnce::new());

        let set_once = arc.clone();
        tokio::spawn(async move {
            set_once.set(detect().await).unwrap();
        });

        Self(DesiredTargetsInner::AutoDetect(arc))
    }

    pub async fn get(&self) -> &[String] {
        use DesiredTargetsInner::*;

        match &self.0 {
            Initialized(targets) => targets,
            AutoDetect(set_once) => &set_once.wait().await.targets,
        }
    }

    /// If `DesiredTargets` is provided with a list of desired targets instead
    /// of detecting the targets, then this function would return `Some`.
    pub fn get_initialized(&self) -> Option<&[String]> {
        use DesiredTargetsInner::*;

        match &self.0 {
            Initialized(targets) => Some(targets),
            AutoDetect(..) => None,
        }
    }

    /// The host the targets were detected from, if they were.
    pub async fn host(&self) -> Option<&Host> {
        match &self.0 {
            DesiredTargetsInner::Initialized(_) => None,
            DesiredTargetsInner::AutoDetect(set_once) => Some(&set_once.wait().await.host),
        }
    }

    /// Check whether the executable at `path`, a binary for one of the
    /// targets, can run on this host, see [`Host::check`].
    ///
    /// Targets given explicitly are not checked, as they may be meant for
    /// another machine, e.g. when installing into a sysroot: this then
    /// always returns `Ok`.
    pub async fn check_binary(&self, path: &Path) -> Result<(), Incompatible> {
        match self.host().await {
            Some(host) => host.check(path).await,
            None => Ok(()),
        }
    }
}

/// If opts_targets is `Some`, then it will be used.
/// Otherwise, call `detect_targets` using `tokio::spawn` to detect targets.
///
/// Since `detect_targets` internally spawns a process and wait for it,
/// it's pretty costy, it is recommended to run this fn ASAP and
/// reuse the result.
pub fn get_desired_targets(opts_targets: Option<Vec<String>>) -> DesiredTargets {
    if let Some(targets) = opts_targets {
        DesiredTargets::initialized(targets)
    } else {
        DesiredTargets::auto_detect()
    }
}
