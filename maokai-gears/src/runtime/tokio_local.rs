extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::pin::Pin;
use downcast::Downcast;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Reconciler, Ticket};
use tokio::task::{JoinHandle, spawn_local};

use crate::ops::task::*;
#[cfg(feature = "tokio-mt-task")]
use crate::runtime::tokio_mt::SendTask;

type LocalTaskFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;
pub type LocalTask = Box<dyn FnOnce(LocalTaskEmitter) -> LocalTaskFuture + 'static>;
type LocalTaskMailbox = Rc<RefCell<VecDeque<Box<dyn maokai_reconciler::Operation + Send>>>>;

#[derive(Clone)]
pub struct LocalTaskMailboxSender(LocalTaskMailbox);

pub struct LocalTaskEmitter {
    sender: LocalTaskMailboxSender,
}

impl LocalTaskEmitter {
    pub fn new(sender: LocalTaskMailboxSender) -> Self {
        Self { sender }
    }

    pub fn emit<Op>(&self, op: Op)
    where
        Op: Operation + Send + 'static,
    {
        self.sender.0.borrow_mut().push_back(Box::new(op));
    }
}

pub struct LocalTaskMailboxSource(LocalTaskMailbox);

fn task_channel() -> (LocalTaskMailboxSender, LocalTaskMailboxSource) {
    let q = Rc::new(RefCell::new(VecDeque::new()));
    (LocalTaskMailboxSender(q.clone()), LocalTaskMailboxSource(q))
}

pub struct TokioLocalTaskConsumer {
    running: BTreeMap<TaskHandle, JoinHandle<()>>,
    sender: LocalTaskMailboxSender,
    source: LocalTaskMailboxSource,
}

impl Default for TokioLocalTaskConsumer {
    fn default() -> Self {
        let (sender, source) = task_channel();
        Self {
            running: BTreeMap::new(),
            sender,
            source,
        }
    }
}

impl OpConsumer for TokioLocalTaskConsumer {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<TaskOp<LocalTask>>::downcast(op) {
            Ok(task_op) => {
                match *task_op {
                    TaskOp::Start { handle, task } => {
                        let running = spawn_local(task(LocalTaskEmitter::new(self.sender.clone())));
                        self.running.insert(handle, running);
                    }
                    TaskOp::Stop(handle) => {
                        if let Some(running) = self.running.remove(&handle) {
                            running.abort();
                        } else {
                            #[cfg(feature = "tokio-mt-task")]
                            {
                                let op: Box<dyn Operation> =
                                    Box::new(TaskOp::<SendTask>::Stop(handle));
                                return OpFlow::Continue(op);
                            }
                        }
                    }
                }
                OpFlow::Consumed
            }
            Err(err) => OpFlow::Continue(err.into_object()),
        }
    }

    fn drain(&mut self, reconciler: &mut Reconciler) -> bool {
        let mut drained = false;
        while let Some(op) = self.source.0.borrow_mut().pop_front() {
            drained = true;
            let op: Box<dyn Operation> = op;
            let _ = reconciler.stage_boxed(op, None);
        }
        drained
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    extern crate alloc;
    extern crate std;

    use super::*;
    use downcast::Downcast;

    struct TestOp(i32);

    impl maokai_reconciler::Operation for TestOp {}

    #[tokio::test]
    async fn local_task_emits_op() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (sender, receiver) = task_channel();

                let task: LocalTask = Box::new(|emitter| {
                    Box::pin(async move {
                        emitter.emit(TestOp(42));
                    })
                });

                let join = spawn_local(task(LocalTaskEmitter::new(sender)));
                join.await.unwrap();

                let op = receiver.0.borrow_mut().pop_front().unwrap();
                let op: Box<dyn maokai_reconciler::Operation> = op;
                let value = Downcast::<TestOp>::downcast(op).unwrap();
                assert_eq!(value.0, 42);
                assert!(receiver.0.borrow_mut().pop_front().is_none());
            })
            .await;
    }

    #[tokio::test]
    async fn local_task_abort() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (sender, receiver) = task_channel();

                let task: LocalTask = Box::new(|_| {
                    Box::pin(async {
                        tokio::time::sleep(std::time::Duration::from_secs(999)).await;
                    })
                });

                let join = spawn_local(task(LocalTaskEmitter::new(sender)));
                join.abort();

                tokio::task::yield_now().await;
                assert!(receiver.0.borrow_mut().pop_front().is_none());
            })
            .await;
    }
}
