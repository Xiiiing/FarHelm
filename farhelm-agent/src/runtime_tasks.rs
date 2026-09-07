//! Fixed service lanes and bounded execution workers keep SQLite off the I/O executor.
use std::{
    future::Future,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinHandle,
};

#[derive(Clone)]
pub struct RuntimeTasks {
    stop: watch::Sender<bool>,
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    turns: Arc<Semaphore>,
    controls: Arc<Semaphore>,
}

impl RuntimeTasks {
    pub fn new() -> Self {
        Self {
            stop: watch::channel(false).0,
            handles: Arc::default(),
            turns: Arc::new(Semaphore::new(4)),
            controls: Arc::new(Semaphore::new(2)),
        }
    }

    pub fn permit(&self, control: bool) -> Option<OwnedSemaphorePermit> {
        if control { &self.controls } else { &self.turns }
            .clone()
            .try_acquire_owned()
            .ok()
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let mut stop = self.stop.subscribe();
        let handle = tokio::spawn(async move {
            if *stop.borrow() {
                return;
            }
            tokio::select! { () = future => {}, _ = stop.changed() => {} }
        });
        let mut handles = self.handles.lock().expect("runtime task registry poisoned");
        handles.retain(|handle| !handle.is_finished());
        handles.push(handle);
    }

    pub async fn shutdown(&self) {
        self.stop.send_replace(true);
        let handles =
            std::mem::take(&mut *self.handles.lock().expect("runtime task registry poisoned"));
        for handle in handles {
            let _ = handle.await;
        }
    }
}

static DATABASE_PERMITS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(8)));
/// Only synchronous work occupies this bounded executor, never network futures.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let permit = DATABASE_PERMITS.clone().acquire_owned().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await?
}

pub async fn watch_database(path: std::path::PathBuf, wake: Arc<tokio::sync::Notify>) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let Some(parent) = path.parent() else { return };
    let parent = if parent.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        parent
    };
    let Ok(name) = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes()) else {
        return;
    };
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        return;
    }
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe {
        libc::inotify_add_watch(
            raw,
            name.as_ptr(),
            libc::IN_MODIFY | libc::IN_CLOSE_WRITE | libc::IN_CREATE,
        )
    } < 0
    {
        return;
    }
    let Ok(fd) = tokio::io::unix::AsyncFd::new(owned) else {
        return;
    };
    let filename = path.file_name().unwrap_or_default().as_encoded_bytes();
    loop {
        let Ok(mut ready) = fd.readable().await else {
            return;
        };
        let read = ready.try_io(|fd| {
            let mut bytes = [0u8; 8192];
            let size = unsafe {
                libc::read(
                    fd.get_ref().as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if size < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut offset = 0;
            let mut changed = false;
            while offset + 16 <= size as usize {
                let len = u32::from_ne_bytes(
                    bytes[offset + 12..offset + 16]
                        .try_into()
                        .expect("inotify name length"),
                ) as usize;
                if offset + 16 + len > size as usize {
                    break;
                }
                let name = &bytes[offset + 16..offset + 16 + len];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                changed |=
                    name == filename || (name.starts_with(filename) && name.ends_with(b"-wal"));
                offset += 16 + len;
            }
            Ok(changed)
        });
        match read {
            Ok(Ok(true)) => wake.notify_one(),
            Ok(Err(_)) => return,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn blocking_lane_does_not_stall_other_lanes_and_turn_capacity_is_bounded() {
        let tasks = RuntimeTasks::new();
        let permits = (0..4)
            .map(|_| tasks.permit(false).unwrap())
            .collect::<Vec<_>>();
        assert!(tasks.permit(false).is_none());
        assert!(tasks.permit(true).is_some());
        let (ready, started) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let _ = ready.send(());
            let _ = blocking(|| {
                std::thread::sleep(Duration::from_millis(150));
                Ok(())
            })
            .await;
        });
        started.await.unwrap();
        let (done, receipt) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = done.send(());
        });
        tokio::time::timeout(Duration::from_millis(100), receipt)
            .await
            .unwrap()
            .unwrap();
        drop(permits);
        assert!(tasks.permit(false).is_some());
        tasks.shutdown().await;
    }
}
