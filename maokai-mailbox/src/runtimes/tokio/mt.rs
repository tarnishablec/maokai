use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::MailboxRuntime;

pub struct SendTaskBox<E: 'static>(Box<dyn Task<Event = E> + Send + Sync>);

impl<E: 'static> SendTaskBox<E> {
    pub fn new<T>(task: T) -> Self
    where
        T: Task<Event = E> + Send + Sync,
    {
        SendTaskBox(Box::new(task))
    }
}

pub struct TokioMtRuntime<E: Send + 'static> {
    tx: tokio::sync::mpsc::UnboundedSender<(TaskHandle, E)>,
    rx: tokio::sync::mpsc::UnboundedReceiver<(TaskHandle, E)>,
}

impl<E: Send + 'static> TokioMtRuntime<E> {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

impl<E: Send + 'static> Default for TokioMtRuntime<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Send + 'static> MailboxRuntime<E, SendTaskBox<E>> for TokioMtRuntime<E> {
    type Running = tokio::task::JoinHandle<()>;

    fn start(&mut self, handle: TaskHandle, task: SendTaskBox<E>) -> Self::Running {
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let event = task.0.run().await;
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

    use core::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use maokai_task::Task;

    use super::*;
    use crate::runtimes::test_support::{Event, run_mailbox_case};

    static SEND_TASK_RUNS: AtomicUsize = AtomicUsize::new(0);

    struct SendEventTask;

    #[async_trait]
    impl Task for SendEventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            SEND_TASK_RUNS.fetch_add(1, Ordering::SeqCst);
            Event::Done
        }
    }

    #[test]
    fn tokio_mt_runtime_runs_send_task_mailbox_case() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread tokio runtime should build");

        SEND_TASK_RUNS.store(0, Ordering::SeqCst);
        runtime.block_on(async {
            run_mailbox_case(TokioMtRuntime::new(), || SendEventTask, SendTaskBox::new).await;
        });

        assert_eq!(SEND_TASK_RUNS.load(Ordering::SeqCst), 1);
    }
}
