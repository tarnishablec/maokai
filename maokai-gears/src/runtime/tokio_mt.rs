extern crate alloc;

use alloc::boxed::Box;
use core::pin::Pin;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ops::task::*;

type SendTaskFuture<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;
pub type SendTask<O> = Box<dyn FnOnce(SendTaskEmitter<O>) -> SendTaskFuture<O> + Send + 'static>;
pub type SendTaskMailboxSender<O> = mpsc::UnboundedSender<TaskOutput<O>>;
pub type SendTaskMailboxSource<O> = mpsc::UnboundedReceiver<TaskOutput<O>>;
pub type SendTaskEmitter<O> = TaskEmitter<SendTaskMailboxSender<O>, O>;

pub struct SendTaskCtx<Context, O> {
    pub ctx: Context,
    emitter: SendTaskEmitter<O>,
}

impl<Context, O: Send> SendTaskCtx<Context, O> {
    pub fn new(ctx: Context, emitter: SendTaskEmitter<O>) -> Self {
        Self { ctx, emitter }
    }

    pub fn emit<Op>(&self, op: Op)
    where
        Op: maokai_reconciler::Operation + Send + 'static,
    {
        self.emitter.emit(op);
    }
}

fn completion_channel<O>() -> (SendTaskMailboxSender<O>, SendTaskMailboxSource<O>) {
    mpsc::unbounded_channel()
}

impl<O: Send> TaskMailboxSender<O> for mpsc::UnboundedSender<TaskOutput<O>> {
    fn send_op(&self, op: Box<dyn maokai_reconciler::Operation + Send>) {
        let _ = mpsc::UnboundedSender::send(self, TaskOutput::Operation(op));
    }

    fn send_completion(&self, completion: TaskCompletion<O>) {
        let _ = mpsc::UnboundedSender::send(self, TaskOutput::Completion(completion));
    }
}

impl<O> TaskMailboxSource<O> for mpsc::UnboundedReceiver<TaskOutput<O>> {
    fn try_recv(&mut self) -> Option<TaskOutput<O>> {
        mpsc::UnboundedReceiver::try_recv(self).ok()
    }
}

pub struct TokioMtRuntime;

impl<O: Send + 'static> TaskRuntime<SendTask<O>> for TokioMtRuntime {
    type Running = JoinHandle<()>;
    type Output = O;
    type Sender = SendTaskMailboxSender<O>;
    type Source = SendTaskMailboxSource<O>;

    fn create_channel(&self) -> (Self::Sender, Self::Source) {
        completion_channel()
    }

    fn start(
        &mut self,
        handle: TaskHandle,
        task: SendTask<O>,
        sender: Self::Sender,
    ) -> Self::Running {
        tokio::spawn(async move {
            let output = task(SendTaskEmitter::new(sender.clone())).await;
            TaskMailboxSender::send_completion(&sender, TaskCompletion { handle, output });
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

        let handle = TaskHandle::next();
        let task: SendTask<i32> = Box::new(|_| Box::pin(async { 42 }));

        let join = runtime.start(handle, task, sender);
        join.await.unwrap();

        let completion = match TaskMailboxSource::try_recv(&mut receiver).unwrap() {
            TaskOutput::Completion(completion) => completion,
            TaskOutput::Operation(_) => panic!("expected completion"),
        };
        assert_eq!(completion.output, 42);
        assert!(TaskMailboxSource::try_recv(&mut receiver).is_none());
    }

    #[tokio::test]
    async fn mt_task_abort() {
        let mut runtime = TokioMtRuntime;
        let (sender, mut receiver) = completion_channel();

        let handle = TaskHandle::next();
        let task: SendTask<()> = Box::new(|_| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(999)).await;
            })
        });

        let join = runtime.start(handle, task, sender);
        <TokioMtRuntime as TaskRuntime<SendTask<()>>>::stop(&mut runtime, handle, join);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(TaskMailboxSource::<()>::try_recv(&mut receiver).is_none());
    }
}
