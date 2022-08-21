use crate::detect_targets;

use std::sync::Arc;

use tokio::sync::OnceCell;

#[derive(Debug)]
enum DesiredTargetsInner {
    AutoDetect(Arc<OnceCell<Vec<String>>>),
    Initialized(Vec<String>),
}

#[derive(Debug)]
pub struct DesiredTargets(DesiredTargetsInner);

impl DesiredTargets {
    fn initialized(targets: Vec<String>) -> Self {
        Self(DesiredTargetsInner::Initialized(targets))
    }

    fn auto_detect() -> Self {
        let arc = Arc::new(OnceCell::new());

        let once_cell = arc.clone();
        tokio::spawn(async move {
            once_cell.get_or_init(detect_targets).await;
        });

        Self(DesiredTargetsInner::AutoDetect(arc))
    }

    pub async fn get(&self) -> &[String] {
        use DesiredTargetsInner::*;

        match &self.0 {
            Initialized(targets) => targets,

            // This will mostly just wait for the spawned task,
            // on rare occausion though, it will poll the future
            // returned by `detect_targets`.
            AutoDetect(once_cell) => once_cell.get_or_init(detect_targets).await,
        }
    }
}

/// If opts_targets is `Some`, then it will be parsed in the format of
/// `$target1,$target2,...`.
/// Otherwise, call `detect_targets` using `tokio::spawn` to detect targets.
///
/// Since `detect_targets` internally spawns a process and wait for it,
/// it's pretty costy, it is recommended to run this fn ASAP and
/// reuse the result.
pub fn get_desired_targets(opts_targets: &Option<String>) -> DesiredTargets {
    if let Some(targets) = opts_targets.as_ref() {
        DesiredTargets::initialized(targets.split(',').map(|t| t.to_string()).collect())
    } else {
        DesiredTargets::auto_detect()
    }
}
