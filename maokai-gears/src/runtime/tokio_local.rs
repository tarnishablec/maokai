extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::pin::Pin;
use tokio::task::{JoinHandle, spawn_local};

use crate::ops::task::*;

type LocalTaskFuture<O> = Pin<Box<dyn Future<Output = O> + 'static>>;
pub type LocalTask<O> = Box<dyn FnOnce(LocalTaskEmitter<O>) -> LocalTaskFuture<O> + 'static>;
type LocalTaskMailbox<O> = Rc<RefCell<VecDeque<TaskOutput<O>>>>;

pub struct LocalTaskMailboxSender<O>(LocalTaskMailbox<O>);

impl<O> Clone for LocalTaskMailboxSender<O> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub struct LocalTaskMailboxSource<O>(LocalTaskMailbox<O>);

pub type LocalTaskEmitter<O> = TaskEmitter<LocalTaskMailboxSender<O>, O>;

impl<O> TaskMailboxSender<O> for LocalTaskMailboxSender<O> {
    fn send_op(&self, op: Box<dyn maokai_reconciler::Operation + Send>) {
        self.0.borrow_mut().push_back(TaskOutput::Operation(op));
    }

    fn send_completion(&self, completion: TaskCompletion<O>) {
        self.0
            .borrow_mut()
            .push_back(TaskOutput::Completion(completion));
    }
}

impl<O> TaskMailboxSource<O> for LocalTaskMailboxSource<O> {
    fn try_recv(&mut self) -> Option<TaskOutput<O>> {
        self.0.borrow_mut().pop_front()
    }
}

fn completion_channel<O>() -> (LocalTaskMailboxSender<O>, LocalTaskMailboxSource<O>) {
    let q = Rc::new(RefCell::new(VecDeque::new()));
    (LocalTaskMailboxSender(q.clone()), LocalTaskMailboxSource(q))
}

pub struct TokioLocalRuntime;

impl<O: 'static> TaskRuntime<LocalTask<O>> for TokioLocalRuntime {
    type Running = JoinHandle<()>;
    type Output = O;
    type Sender = LocalTaskMailboxSender<O>;
    type Source = LocalTaskMailboxSource<O>;

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
            let output = task(LocalTaskEmitter::new(sender.clone())).await;
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
    async fn local_task_completes() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut runtime = TokioLocalRuntime;
                let (sender, mut receiver) = completion_channel();

                let handle = TaskHandle::next();
                let task: LocalTask<i32> = Box::new(|_| Box::pin(async { 42 }));

                let join = runtime.start(handle, task, sender);
                join.await.unwrap();

                let completion = match receiver.try_recv().unwrap() {
                    TaskOutput::Completion(completion) => completion,
                    TaskOutput::Operation(_) => panic!("expected completion"),
                };
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
                let task: LocalTask<()> = Box::new(|_| {
                    Box::pin(async {
                        tokio::time::sleep(std::time::Duration::from_secs(999)).await;
                    })
                });

                let join = runtime.start(handle, task, sender);
                <TokioLocalRuntime as TaskRuntime<LocalTask<()>>>::stop(&mut runtime, handle, join);

                tokio::task::yield_now().await;
                assert!(receiver.try_recv().is_none());
            })
            .await;
    }
}
