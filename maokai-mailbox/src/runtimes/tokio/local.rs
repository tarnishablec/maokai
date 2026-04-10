use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::MailboxRuntime;

pub type LocalTaskBox<E> = Box<dyn Task<Event = E>>;

pub struct TokioLocalRuntime<E: 'static> {
    tx: tokio::sync::mpsc::UnboundedSender<(TaskHandle, E)>,
    rx: tokio::sync::mpsc::UnboundedReceiver<(TaskHandle, E)>,
}

impl<E: 'static> TokioLocalRuntime<E> {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

impl<E: 'static> Default for TokioLocalRuntime<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: 'static> MailboxRuntime<E, LocalTaskBox<E>> for TokioLocalRuntime<E> {
    type Running = tokio::task::JoinHandle<()>;

    fn start(&mut self, handle: TaskHandle, task: LocalTaskBox<E>) -> Self::Running {
        let tx = self.tx.clone();

        // `Task` is not required to be `Send`, so the local Tokio runtime uses `spawn_local`.
        tokio::task::spawn_local(async move {
            let event = task.run().await;
            let _ = tx.send((handle, event));
        })
    }

    fn stop(&mut self, running: Self::Running) {
        running.abort();
    }

    fn poll(&mut self) -> Option<(TaskHandle, E)> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::boxed::Box;
    use core::marker::PhantomData;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use maokai_task::Task;

    use super::*;
    use crate::runtimes::test_support::{Event, run_mailbox_case};

    static SEND_TASK_RUNS: AtomicUsize = AtomicUsize::new(0);
    static LOCAL_TASK_RUNS: AtomicUsize = AtomicUsize::new(0);

    struct SendEventTask;
    struct LocalEventTask {
        marker: PhantomData<*const ()>,
    }

    unsafe impl Sync for LocalEventTask {}

    #[async_trait]
    impl Task for SendEventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            SEND_TASK_RUNS.fetch_add(1, Ordering::SeqCst);
            Event::Done
        }
    }

    #[async_trait]
    impl Task for LocalEventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            LOCAL_TASK_RUNS.fetch_add(1, Ordering::SeqCst);
            Event::Done
        }
    }

    fn make_send_task() -> SendEventTask {
        SendEventTask
    }

    fn make_local_task() -> LocalEventTask {
        LocalEventTask {
            marker: PhantomData,
        }
    }

    #[test]
    fn tokio_local_runtime_runs_send_task_mailbox_case() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime should build");
        let local = tokio::task::LocalSet::new();

        SEND_TASK_RUNS.store(0, Ordering::SeqCst);
        local.block_on(&runtime, async {
            run_mailbox_case(TokioLocalRuntime::new(), make_send_task, |task| {
                Box::new(task)
            })
            .await;
        });

        assert_eq!(SEND_TASK_RUNS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tokio_local_runtime_runs_non_send_task_mailbox_case() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime should build");
        let local = tokio::task::LocalSet::new();

        LOCAL_TASK_RUNS.store(0, Ordering::SeqCst);
        local.block_on(&runtime, async {
            run_mailbox_case(TokioLocalRuntime::new(), make_local_task, |task| {
                Box::new(task)
            })
            .await;
        });

        assert_eq!(LOCAL_TASK_RUNS.load(Ordering::SeqCst), 1);
    }
}
