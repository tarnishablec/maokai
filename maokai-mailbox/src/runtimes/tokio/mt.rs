use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::MailboxRuntime;

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

impl<E: Send + 'static> MailboxRuntime<E> for TokioMtRuntime<E> {
    type Running = tokio::task::JoinHandle<()>;
    type Task = dyn Task<Event = E> + Send + 'static;

    fn start(&mut self, handle: TaskHandle, task: Box<Self::Task>) -> Self::Running {
        let tx = self.tx.clone();

        tokio::spawn(async move {
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
