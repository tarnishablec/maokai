extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use tokio::task::{JoinHandle, spawn_local};

use crate::ops::task::*;

pub type LocalTask<O> = Pin<Box<dyn Future<Output = O> + 'static>>;

pub struct TokioLocalRuntime;

impl<O: 'static> TaskRuntime<LocalTask<O>> for TokioLocalRuntime {
    type Running = JoinHandle<()>;
    type Output = O;
    type Sender = Rc<RefCell<VecDeque<TaskCompletion<O>>>>;
    type Receiver = Rc<RefCell<VecDeque<TaskCompletion<O>>>>;

    fn create_channel(&self) -> (Self::Sender, Self::Receiver) {
        let q = Rc::new(RefCell::new(VecDeque::new()));
        (q.clone(), q)
    }

    fn start(
        &mut self,
        handle: TaskHandle,
        task: LocalTask<O>,
        sender: Self::Sender,
    ) -> Self::Running {
        spawn_local(async move {
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
    async fn local_task_completes() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut runtime = TokioLocalRuntime;
                let (sender, mut receiver) = <TokioLocalRuntime as TaskRuntime<LocalTask<i32>>>::create_channel(&runtime);

                let handle = TaskHandle::from_raw(0);
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
                let (sender, mut receiver) = <TokioLocalRuntime as TaskRuntime<LocalTask<()>>>::create_channel(&runtime);

                let handle = TaskHandle::from_raw(0);
                let task: LocalTask<()> = Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_secs(999)).await;
                });

                let join = runtime.start(handle, task, sender);
                <TokioLocalRuntime as TaskRuntime<LocalTask<()>>>::stop(
                    &mut runtime, handle, join,
                );

                tokio::task::yield_now().await;
                assert!(receiver.try_recv().is_none());
            })
            .await;
    }
}
