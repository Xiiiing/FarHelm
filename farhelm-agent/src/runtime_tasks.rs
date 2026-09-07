//! Fixed service lanes and bounded execution workers keep SQLite off the I/O executor.
use std::{
    future::Future,
    sync::{Arc, Mutex},
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
        let runtime = tokio::runtime::Handle::current();
        let handle = tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                if *stop.borrow() {
                    return;
                }
                tokio::select! { () = future => {}, _ = stop.changed() => {} }
            })
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
            std::thread::sleep(Duration::from_millis(150));
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
