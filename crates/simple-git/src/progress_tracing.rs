use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use compact_str::{format_compact, CompactString};
use gix::progress::{
    prodash::messages::MessageLevel, Count, Id, NestedProgress, Progress, Step, StepShared, Unit,
    UNKNOWN,
};
use tokio::time;
use tracing::{error, info};

pub(super) struct TracingProgress {
    name: CompactString,
    id: Id,
    max: Option<usize>,
    unit: Option<Unit>,
    step: StepShared,
    trigger: Arc<AtomicBool>,
}

const EMIT_LOG_EVERY_S: f32 = 0.5;
const SEP: &str = "::";

impl TracingProgress {
    /// Create a new instanCompactce from `name`.
    pub fn new(name: &str) -> Self {
        let trigger = Arc::new(AtomicBool::new(true));
        tokio::spawn({
            let mut interval = time::interval(Duration::from_secs_f32(EMIT_LOG_EVERY_S));
            interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

            let trigger = Arc::clone(&trigger);
            async move {
                while Arc::strong_count(&trigger) > 1 {
                    trigger.store(true, Ordering::Relaxed);

                    interval.tick().await;
                }
            }
        });
        Self {
            name: CompactString::new(name),
            id: UNKNOWN,
            max: None,
            step: Default::default(),
            unit: None,
            trigger,
        }
    }

    fn log_progress(&self, step: Step) {
        if self.trigger.swap(false, Ordering::Relaxed) {
            match (self.max, &self.unit) {
                (max, Some(unit)) => {
                    info!("{} → {}", self.name, unit.display(step, max, None))
                }
                (Some(max), None) => info!("{} → {} / {}", self.name, step, max),
                (None, None) => info!("{} → {}", self.name, step),
            }
        }
    }
}

impl Count for TracingProgress {
    fn set(&self, step: Step) {
        self.step.store(step, Ordering::Relaxed);
        self.log_progress(step);
    }

    fn step(&self) -> Step {
        self.step.load(Ordering::Relaxed)
    }

    fn inc_by(&self, step_to_inc: Step) {
        self.log_progress(
            self.step
                .fetch_add(step_to_inc, Ordering::Relaxed)
                .wrapping_add(step_to_inc),
        );
    }

    fn counter(&self) -> StepShared {
        Arc::clone(&self.step)
    }
}

impl Progress for TracingProgress {
    fn init(&mut self, max: Option<Step>, unit: Option<Unit>) {
        self.max = max;
        self.unit = unit;
    }

    fn unit(&self) -> Option<Unit> {
        self.unit.clone()
    }

    fn max(&self) -> Option<usize> {
        self.max
    }

    fn set_max(&mut self, max: Option<Step>) -> Option<Step> {
        let prev = self.max;
        self.max = max;
        prev
    }

    fn set_name(&mut self, name: String) {
        self.name = self
            .name
            .split("::")
            .next()
            .map(|parent| format_compact!("{parent}{SEP}{name}"))
            .unwrap_or_else(|| name.into());
    }

    fn name(&self) -> Option<String> {
        self.name.split(SEP).nth(1).map(ToOwned::to_owned)
    }

    fn id(&self) -> Id {
        self.id
    }

    fn message(&self, level: MessageLevel, message: String) {
        let name = &self.name;
        match level {
            MessageLevel::Info => info!("ℹ{name} → {message}"),
            MessageLevel::Failure => error!("𐄂{name} → {message}"),
            MessageLevel::Success => info!("✓{name} → {message}"),
        }
    }
}

impl NestedProgress for TracingProgress {
    type SubProgress = TracingProgress;

    fn add_child(&mut self, name: impl Into<String>) -> Self::SubProgress {
        self.add_child_with_id(name, UNKNOWN)
    }

    fn add_child_with_id(&mut self, name: impl Into<String>, id: Id) -> Self::SubProgress {
        Self {
            name: format_compact!("{}{}{}", self.name, SEP, Into::<String>::into(name)),
            id,
            step: Arc::default(),
            max: None,
            unit: None,
            trigger: Arc::clone(&self.trigger),
        }
    }
}
