extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use maokai_reconciler::Ticket;
use tokio::task::{JoinHandle, spawn_local};

use crate::ops::task::*;

pub type LocalTask<O> = Pin<Box<dyn Future<Output = O> + 'static>>;
type LocalTaskCompletionQueue<O> = Rc<RefCell<VecDeque<TaskCompletion<O>>>>;

pub struct LocalTaskCompletionSender<O>(LocalTaskCompletionQueue<O>);

impl<O> Clone for LocalTaskCompletionSender<O> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct LocalTaskCompletionSource<O>(LocalTaskCompletionQueue<O>);

impl<O> TaskCompletionSender<O> for LocalTaskCompletionSender<O> {
    fn send(&self, completion: TaskCompletion<O>) {
        self.0.borrow_mut().push_back(completion);
    }
}

impl<O> TaskCompletionSource<O> for LocalTaskCompletionSource<O> {
    fn try_recv(&mut self) -> Option<TaskCompletion<O>> {
        self.0.borrow_mut().pop_front()
    }
}

fn completion_channel<O>() -> (LocalTaskCompletionSender<O>, LocalTaskCompletionSource<O>) {
    let q = Rc::new(RefCell::new(VecDeque::new()));
    (
        LocalTaskCompletionSender(q.clone()),
        LocalTaskCompletionSource(q),
    )
}

pub trait LocalTaskOpsExt<O: 'static>: TaskOpsExt<LocalTask<O>> {
    fn start_local_task<F>(&mut self, future: F) -> Option<TaskHandle>
    where
        F: Future<Output = O> + 'static,
    {
        self.start_task(Box::pin(future) as LocalTask<O>)
    }

    fn stop_local_task(&mut self, handle: TaskHandle) -> Option<Ticket> {
        self.stop_task(handle)
    }
}

impl<O: 'static, Ctx> LocalTaskOpsExt<O> for Ctx where Ctx: TaskOpsExt<LocalTask<O>> {}

pub struct TokioLocalRuntime;

impl<O: 'static> TaskRuntime<LocalTask<O>> for TokioLocalRuntime {
    type Running = JoinHandle<()>;
    type Output = O;
    type Sender = LocalTaskCompletionSender<O>;
    type Source = LocalTaskCompletionSource<O>;

    fn create_channel(&self) -> (Self::Sender, Self::Source) {
        completion_channel()
    }

    fn start(
        &mut self,
        handle: TaskHandle,
        task: LocalTask<O>,
        sender: Self::Sender,
    ) -> Self::Running {
        spawn_local(async move {
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
    async fn local_task_completes() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut runtime = TokioLocalRuntime;
                let (sender, mut receiver) = completion_channel();

                let handle = TaskHandle::next();
                let task: LocalTask<i32> = Box::pin(async { 42 });

                let join = runtime.start(handle, task, sender);
                join.await.unwrap();

                let completion = receiver.try_recv().unwrap();
                assert_eq!(completion.output, 42);
                assert!(receiver.try_recv().is_none());
            })
            .await;
    }

    #[tokio::test]
    async fn local_task_abort() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut runtime = TokioLocalRuntime;
                let (sender, mut receiver) = completion_channel();

                let handle = TaskHandle::next();
                let task: LocalTask<()> = Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_secs(999)).await;
                });

                let join = runtime.start(handle, task, sender);
                <TokioLocalRuntime as TaskRuntime<LocalTask<()>>>::stop(&mut runtime, handle, join);

                tokio::task::yield_now().await;
                assert!(receiver.try_recv().is_none());
            })
            .await;
    }
}
