use std::{
    io::{self, BufRead, StdinLock, Write},
    sync::mpsc,
};

use tokio::{sync::oneshot, task::spawn_blocking};

use crate::{binstall::Resolution, BinstallError};

#[derive(Debug, Clone)]
pub enum Request {
    Prompt,
    Start,
    Stop,
    ProgressSetup { steps: usize, name: String },
    ProgressStep { status: String },
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
        // This task should be the only one able to access stdin
        let mut stdin = io::stdin().lock();
        let mut unblock = Some(cs);

        for req in rr {
            use Request::*;
            match req {
                Start => todo!("start timer"),
                Stop => todo!("stop timer"),
                ProgressSetup { steps, name } => {
                    todo!("setup {steps}-step progress bar for {name}")
                }
                ProgressStep { status } => {
                    todo!("advance current progress bar by one with status {status}")
                }
                Prompt => {
                    if let Some(unblock) = unblock.take() {
                        unblock
                            .send(if prompt(&mut stdin) {
                                Ok(())
                            } else {
                                Err(BinstallError::UserAbort)
                            })
                            .unwrap();
                    } else {
                        unreachable!("confirmation is single-use");
                    }
                }
            }
        }
    });

    (Controller(rs.clone()), Confirmer { block: cr, req: rs })
}

impl Confirmer {
    pub async fn confirm(
        self,
        _resolutions: &[Resolution],
        show_prompt: bool,
    ) -> Result<(), BinstallError> {
        if show_prompt {
            self.req
                .send(Request::Prompt)
                .map_err(|_| BinstallError::UserAbort)?;
            self.block.await.map_err(|_| BinstallError::UserAbort)??;
        }

        Ok(())
    }
}

impl Controller {
    pub fn start(&self) {
        self.0.send(Request::Start).unwrap();
    }

    pub fn stop(&self) {
        self.0.send(Request::Stop).unwrap();
    }

    pub fn setup(&self, steps: usize, bar_name: &str) {
        self.0
            .send(Request::ProgressSetup {
                steps,
                name: bar_name.into(),
            })
            .unwrap();
    }

    pub fn step(&self, step: &str) {
        self.0
            .send(Request::ProgressStep {
                status: step.into(),
            })
            .unwrap();
    }
}

fn prompt(stdin: &mut StdinLock) -> bool {
    // Lock stdout so that nobody can interfere with confirmation
    let mut stdout = io::stdout().lock();
    let mut input = String::with_capacity(16);

    loop {
        writeln!(&mut stdout, "Do you wish to continue? yes/[no]").unwrap();
        write!(&mut stdout, "? ").unwrap();
        stdout.flush().unwrap();

        input.clear();
        stdin.read_line(&mut input).unwrap();

        match input.as_str().trim() {
            "yes" | "y" | "YES" | "Y" => return true,
            "no" | "n" | "NO" | "N" | "" => return false,
            _ => continue,
        }
    }
}
