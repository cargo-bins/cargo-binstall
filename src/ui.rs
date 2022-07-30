use std::sync::mpsc;

use console::{style, user_attended};
use dialoguer::{theme::ColorfulTheme, Confirm};
use indicatif::{ProgressBar, ProgressStyle};
use itertools::Itertools;
use tokio::{sync::oneshot, task::spawn_blocking};

use crate::{binstall::Resolution, BinstallError};

#[derive(Debug, Clone)]
pub enum Request {
    Display(String),
    Prompt,
    ProgressSetup { steps: u64, name: String },
    ProgressDoing { status: String },
    ProgressDone { status: String },
    Finish,
}

#[derive(Debug, Clone)]
pub struct Controller(mpsc::Sender<Request>);

#[derive(Debug)]
pub struct Confirmer {
    block: oneshot::Receiver<Result<(), BinstallError>>,
    req: mpsc::Sender<Request>,
}

pub fn init() -> (Controller, Confirmer) {
    let (rs, rr) = mpsc::channel();
    let (cs, cr) = oneshot::channel();

    spawn_blocking(move || {
        use Request::*;

        let functional = user_attended();
        let mut unblock = Some(cs);
        let mut current_bar: Option<ProgressBar> = None;
        let mut statuses = Vec::<String>::with_capacity(3);

        for req in rr {
            // Pretend the UI works but do nothing
            if !functional {
                continue;
            }

            match req {
                ProgressSetup { steps, name } => {
                    if let Some(old) = current_bar.take() {
                        old.set_message("done");
                        old.finish();
                    }

                    statuses.truncate(0);
                    let new = ProgressBar::new(steps * 10)
                        .with_style(
                            ProgressStyle::default_bar()
                                .template("{prefix:>12.green} {bar:40.black} {msg:.cyan}")
                                .unwrap(),
                        )
                        .with_prefix(name);

                    new.inc(1);
                    current_bar.replace(new);
                }
                ProgressDoing { status } => {
                    if let Some(ref bar) = current_bar {
                        bar.inc(1);
                        if statuses.is_empty() {
                            bar.set_message(status.clone());
                        }
                        statuses.push(status);
                    }
                }
                ProgressDone { status } => {
                    if let Some(ref bar) = current_bar {
                        statuses.retain(|st| st != &status);
                        bar.inc(10);
                        if let Some(status) = statuses.first() {
                            bar.set_message(status.clone());
                        }
                    }
                }
                Display(text) => {
                    if let Some(old) = current_bar.take() {
                        old.set_message("done");
                        old.finish();
                    }

                    eprintln!("{}", text);
                }
                Prompt => {
                    if let Some(unblock) = unblock.take() {
                        if let Some(old) = current_bar.take() {
                            old.set_message("done");
                            old.finish();
                        }

                        let response = Confirm::with_theme(&ColorfulTheme::default())
                            .with_prompt("Does that look right?")
                            .default(false)
                            .interact()
                            .unwrap();

                        unblock
                            .send(if response {
                                Ok(())
                            } else {
                                Err(BinstallError::UserAbort)
                            })
                            .unwrap();
                    } else {
                        unreachable!("Confirmer is single-use");
                    }
                }
                Finish => {
                    if let Some(old) = current_bar.take() {
                        old.set_message("done");
                        old.finish();
                    }

                    break;
                }
            }
        }
    });

    (Controller(rs.clone()), Confirmer { block: cr, req: rs })
}

impl Confirmer {
    pub async fn confirm(self) -> Result<(), BinstallError> {
        self.req
            .send(Request::Prompt)
            .map_err(|_| BinstallError::UserAbort)?;
        self.block.await.map_err(|_| BinstallError::UserAbort)?
    }
}

impl Controller {
    pub fn setup(&self, steps: usize, bar_name: &str) {
        self.0
            .send(Request::ProgressSetup {
                steps: u64::try_from(steps).unwrap(),
                name: bar_name.into(),
            })
            .unwrap();
    }

    pub fn doing(&self, step: &str) {
        self.0
            .send(Request::ProgressDoing {
                status: step.into(),
            })
            .unwrap();
    }

    pub fn done(&self, step: &str) {
        self.0
            .send(Request::ProgressDone {
                status: step.into(),
            })
            .unwrap();
    }

    pub fn summary(&self, resolutions: &[Resolution], versioned: bool) {
        self.0
            .send(Request::Display(
                resolutions
                    .iter()
                    .map(|res| match res {
                        Resolution::Fetch {
                            fetcher,
                            name,
                            bin_files,
                            ..
                        } => (
                            if fetcher.is_third_party() {
                                format!("{} (3rd party)", fetcher.source_name())
                            } else {
                                fetcher.source_name()
                            },
                            (name, Some(bin_files)),
                        ),
                        Resolution::Source { package } => {
                            ("source".to_string(), (&package.name, None))
                        }
                    })
                    .into_group_map()
                    .into_iter()
                    .sorted_by_key(|(source, _)| {
                        if source == "source" {
                            "\u{FFFFF}"
                        } else {
                            source
                        }
                        .to_string()
                    })
                    .map(|(source, pkgs)| {
                        format!(
                            "{} {}{}\n{}",
                            style("From").blue(),
                            style(source).bold().blue(),
                            style(":").blue(),
                            pkgs.into_iter()
                                .map(|(name, bins)| {
                                    let pkg = style(name).magenta().bold();
                                    if let Some(bins) = bins {
                                        format!(
                                            "\t{} {}{}{}",
                                            pkg,
                                            style("(").magenta(),
                                            bins.into_iter()
                                                .flat_map(|bin| {
                                                    let mut v = vec![bin.main_filename()];
                                                    if versioned {
                                                        v.push(bin.versioned_filename());
                                                    }
                                                    v
                                                })
                                                .join(", "),
                                            style(")").magenta()
                                        )
                                    } else {
                                        format!("\t{}", pkg)
                                    }
                                })
                                .join("\n")
                        )
                    })
                    .join("\n"),
            ))
            .unwrap();
    }

    pub fn finish(&self) {
        self.0.send(Request::Finish).unwrap();
    }
}
