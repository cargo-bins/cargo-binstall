mod detect;
pub use detect::detect_targets;

use std::sync::Arc;

use tokio::sync::OnceCell;

/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");

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

/// If opts_targets is `Some`, then it will be used.
/// Otherwise, call `detect_targets` using `tokio::spawn` to detect targets.
///
/// Since `detect_targets` internally spawns a process and wait for it,
/// it's pretty costy.
///
/// Calling it through `tokio::spawn` would enable other tasks, such as
/// fetching the crate tarballs, to be executed concurrently.
pub fn get_desired_targets(opts_targets: &Option<String>) -> DesiredTargets {
    if let Some(targets) = opts_targets.as_ref() {
        DesiredTargets::initialized(targets.split(',').map(|t| t.to_string()).collect())
    } else {
        DesiredTargets::auto_detect()
    }
}
