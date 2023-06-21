use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use compact_str::{format_compact, CompactString};
use gix::progress::{
    prodash::messages::MessageLevel, Id, Progress, Step, StepShared, Unit, UNKNOWN,
};
use tokio::time;
use tracing::{error, info};

pub(super) struct TracingProgress {
    name: CompactString,
    id: Id,
    max: Option<usize>,
    unit: Option<Unit>,
    step: usize,
    trigger: Arc<AtomicBool>,
}

const EMIT_LOG_EVERY_S: f32 = 0.5;
const SEP: &str = "::";

impl TracingProgress {
    /// Create a new instanCompactce from `name`.
    pub fn new(name: CompactString) -> Self {
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
            name,
            id: UNKNOWN,
            max: None,
            step: 0,
            unit: None,
            trigger,
        }
    }
}

impl Progress for TracingProgress {
    type SubProgress = TracingProgress;

    fn add_child(&mut self, name: impl Into<String>) -> Self::SubProgress {
        self.add_child_with_id(name, UNKNOWN)
    }

    fn add_child_with_id(&mut self, name: impl Into<String>, id: Id) -> Self::SubProgress {
        Self {
            name: format_compact!("{}{}{}", self.name, SEP, Into::<String>::into(name)),
            id,
            step: 0,
            max: None,
            unit: None,
            trigger: Arc::clone(&self.trigger),
        }
    }

    fn init(&mut self, max: Option<usize>, unit: Option<Unit>) {
        self.max = max;
        self.unit = unit;
    }

    fn set(&mut self, step: usize) {
        self.step = step;
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

    fn step(&self) -> usize {
        self.step
    }

    fn inc_by(&mut self, step: usize) {
        self.set(self.step + step)
    }

    fn set_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.name = self
            .name
            .split("::")
            .next()
            .map(|parent| format_compact!("{}{}{}", parent.to_owned(), SEP, name))
            .unwrap_or_else(|| name.into());
    }

    fn name(&self) -> Option<String> {
        self.name.split(SEP).nth(1).map(ToOwned::to_owned)
    }

    fn id(&self) -> Id {
        self.id
    }

    fn message(&self, level: MessageLevel, message: impl Into<String>) {
        let message: String = message.into();
        match level {
            MessageLevel::Info => info!("ℹ{} → {}", self.name, message),
            MessageLevel::Failure => error!("𐄂{} → {}", self.name, message),
            MessageLevel::Success => info!("✓{} → {}", self.name, message),
        }
    }

    fn counter(&self) -> Option<StepShared> {
        None
    }
}
