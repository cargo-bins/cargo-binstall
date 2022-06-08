use std::fs;
use std::io::{self, Write};
use std::path::Path;

use bytes::Bytes;
use tokio::{sync::mpsc, task::spawn_blocking};

use super::AutoAbortJoinHandle;

#[derive(Debug)]
pub struct AsyncFileWriter {
    /// Use AutoAbortJoinHandle so that the task
    /// will be cancelled on failure.
    handle: AutoAbortJoinHandle<io::Result<()>>,
    tx: mpsc::Sender<Bytes>,
}

impl AsyncFileWriter {
    pub fn new(path: &Path) -> io::Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = fs::File::create(path)?;
        let (tx, rx) = mpsc::channel::<Bytes>(100);

        let handle = AutoAbortJoinHandle::new(spawn_blocking(move || {
            // close rx on error so that tx.send will return an error
            let mut rx = scopeguard::guard(rx, |mut rx| {
                rx.close();
            });

            while let Some(bytes) = rx.blocking_recv() {
                file.write_all(&*bytes)?;
            }

            file.flush()?;

            Ok(())
        }));

        Ok(Self { handle, tx })
    }

    /// Upon error, this writer shall not be reused.
    /// Otherwise, `Self::done` would panic.
    pub async fn write(&mut self, bytes: Bytes) -> io::Result<()> {
        if self.tx.send(bytes).await.is_err() {
            // task failed
            Err(Self::wait(&mut self.handle).await.expect_err(
                "Implementation bug: write task finished successfully before all writes are done",
            ))
        } else {
            Ok(())
        }
    }

    pub async fn done(mut self) -> io::Result<()> {
        // Drop tx as soon as possible so that the task would wrap up what it
        // was doing and flush out all the pending data.
        drop(self.tx);

        Self::wait(&mut self.handle).await
    }

    async fn wait(handle: &mut AutoAbortJoinHandle<io::Result<()>>) -> io::Result<()> {
        match handle.await {
            Ok(res) => res,
            Err(join_err) => Err(io::Error::new(io::ErrorKind::Other, join_err)),
        }
    }
}
