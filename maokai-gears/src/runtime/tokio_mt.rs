extern crate alloc;

use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;
use maokai_reconciler::Ticket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ops::task::*;

pub type SendTask<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;
pub type SendTaskCompletionSender<O> = mpsc::UnboundedSender<TaskCompletion<O>>;
pub type SendTaskCompletionSource<O> = mpsc::UnboundedReceiver<TaskCompletion<O>>;

pub fn completion_channel<O>() -> (SendTaskCompletionSender<O>, SendTaskCompletionSource<O>) {
    mpsc::unbounded_channel()
}

pub trait SendTaskOpsExt<O: Send + 'static>: TaskOpsExt<SendTask<O>> {
    fn start_send_task<F>(&mut self, future: F) -> Option<TaskHandle>
    where
        F: Future<Output = O> + Send + 'static,
    {
        self.start_task(Box::pin(future))
    }

    fn stop_send_task(&mut self, handle: TaskHandle) -> Option<Ticket> {
        self.stop_task(handle)
    }
}

impl<O: Send + 'static, Ctx> SendTaskOpsExt<O> for Ctx where Ctx: TaskOpsExt<SendTask<O>> {}

impl<O: Send> TaskCompletionSender<O> for mpsc::UnboundedSender<TaskCompletion<O>> {
    fn send(&self, completion: TaskCompletion<O>) {
        let _ = mpsc::UnboundedSender::send(self, completion);
    }
}

impl<O> TaskCompletionSource<O> for mpsc::UnboundedReceiver<TaskCompletion<O>> {
    fn try_recv(&mut self) -> Option<TaskCompletion<O>> {
        mpsc::UnboundedReceiver::try_recv(self).ok()
    }
}

pub struct TokioMtRuntime;

impl<O: Send + 'static> TaskRuntime<SendTask<O>> for TokioMtRuntime {
    type Running = JoinHandle<()>;
    type Output = O;
    type Sender = SendTaskCompletionSender<O>;

    fn start(
        &mut self,
        handle: TaskHandle,
        task: SendTask<O>,
        sender: Self::Sender,
    ) -> Self::Running {
        tokio::spawn(async move {
            let output = task.await;
            TaskCompletionSender::send(&sender, TaskCompletion { handle, output });
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
        let (sender, mut receiver) = completion_channel();

        let mut handles = TaskHandles::default();
        let handle = handles.alloc();
        let task: SendTask<i32> = Box::pin(async { 42 });

        let join = runtime.start(handle, task, sender);
        join.await.unwrap();

        let completion = TaskCompletionSource::try_recv(&mut receiver).unwrap();
        assert_eq!(completion.output, 42);
        assert!(TaskCompletionSource::try_recv(&mut receiver).is_none());
    }

    #[tokio::test]
    async fn mt_task_abort() {
        let mut runtime = TokioMtRuntime;
        let (sender, mut receiver) = completion_channel();

        let mut handles = TaskHandles::default();
        let handle = handles.alloc();
        let task: SendTask<()> = Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_secs(999)).await;
        });

        let join = runtime.start(handle, task, sender);
        <TokioMtRuntime as TaskRuntime<SendTask<()>>>::stop(&mut runtime, handle, join);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(TaskCompletionSource::<()>::try_recv(&mut receiver).is_none());
    }
}
