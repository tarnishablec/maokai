use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::MailboxRuntime;

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

impl<E: 'static> MailboxRuntime<E> for TokioLocalRuntime<E> {
    type Running = tokio::task::JoinHandle<()>;
    type Task = dyn Task<Event = E> + 'static;

    fn start(&mut self, handle: TaskHandle, task: Box<dyn Task<Event = E>>) -> Self::Running {
        let tx = self.tx.clone();

        // `Task` is not required to be `Send`, so the local Tokio runtime uses `spawn_local`.
        tokio::task::spawn_local(async move {
            let any_event = task.run().await;

            let _ = tx.send((handle, any_event));
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
    
}
