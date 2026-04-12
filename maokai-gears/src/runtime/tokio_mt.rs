extern crate alloc;

use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ops::task::*;

pub type SendTask<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;

impl<O: Send> CompletionSender<O> for mpsc::UnboundedSender<TaskCompletion<O>> {
    fn send(&self, completion: TaskCompletion<O>) {
        let _ = mpsc::UnboundedSender::send(self, completion);
    }
}

impl<O> CompletionReceiver<O> for mpsc::UnboundedReceiver<TaskCompletion<O>> {
    fn try_recv(&mut self) -> Option<TaskCompletion<O>> {
        mpsc::UnboundedReceiver::try_recv(self).ok()
    }
}

pub struct TokioMtRuntime;

impl<O: Send + 'static> TaskRuntime<SendTask<O>> for TokioMtRuntime {
    type Running = JoinHandle<()>;
    type Output = O;
    type Sender = mpsc::UnboundedSender<TaskCompletion<O>>;
    type Receiver = mpsc::UnboundedReceiver<TaskCompletion<O>>;

    fn create_channel(&self) -> (Self::Sender, Self::Receiver) {
        mpsc::unbounded_channel()
    }

    fn start(
        &mut self,
        handle: TaskHandle,
        task: SendTask<O>,
        sender: Self::Sender,
    ) -> Self::Running {
        tokio::spawn(async move {
            let output = task.await;
            CompletionSender::send(&sender, TaskCompletion { handle, output });
        })
    }

    fn stop(&mut self, _handle: TaskHandle, running: Self::Running) {
        running.abort();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    extern crate alloc;
    extern crate std;

    use super::*;

    #[tokio::test]
    async fn mt_task_completes() {
        let mut runtime = TokioMtRuntime;
        let (sender, mut receiver) =
            <TokioMtRuntime as TaskRuntime<SendTask<i32>>>::create_channel(&runtime);

        let handle = TaskHandle::from_raw(0);
        let task: SendTask<i32> = Box::pin(async { 42 });

        let join = runtime.start(handle, task, sender);
        join.await.unwrap();

        let completion = CompletionReceiver::try_recv(&mut receiver).unwrap();
        assert_eq!(completion.output, 42);
        assert!(CompletionReceiver::try_recv(&mut receiver).is_none());
    }

    #[tokio::test]
    async fn mt_task_abort() {
        let mut runtime = TokioMtRuntime;
        let (sender, mut receiver) =
            <TokioMtRuntime as TaskRuntime<SendTask<()>>>::create_channel(&runtime);

        let handle = TaskHandle::from_raw(0);
        let task: SendTask<()> = Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_secs(999)).await;
        });

        let join = runtime.start(handle, task, sender);
        <TokioMtRuntime as TaskRuntime<SendTask<()>>>::stop(&mut runtime, handle, join);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(CompletionReceiver::<()>::try_recv(&mut receiver).is_none());
    }
}
