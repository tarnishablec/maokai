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

type LocalTaskFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;
pub type LocalTask = Box<dyn FnOnce(LocalTaskEmitter) -> LocalTaskFuture + 'static>;
type LocalTaskMailbox = Rc<RefCell<VecDeque<Box<dyn Operation + Send>>>>;

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

struct LocalTaskRuntime {
    running: BTreeMap<TaskHandle, JoinHandle<()>>,
    sender: LocalTaskMailboxSender,
    source: LocalTaskMailboxSource,
}

pub struct TokioLocalTaskConsumer {
    runtime: LocalTaskRuntime,
}

impl Default for TokioLocalTaskConsumer {
    fn default() -> Self {
        let (sender, source) = task_channel();
        Self {
            runtime: LocalTaskRuntime {
                running: BTreeMap::new(),
                sender,
                source,
            },
        }
    }
}

impl OpConsumer for TokioLocalTaskConsumer {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<StartTaskOp<LocalTask>>::downcast(op) {
            Ok(task_op) => {
                let StartTaskOp { handle, task } = *task_op;
                let sender = self.runtime.sender.clone();
                let running = spawn_local(task(LocalTaskEmitter::new(sender)));
                self.runtime.running.insert(handle, running);
                OpFlow::Consumed
            }
            Err(op) => match Downcast::<StopTaskOp>::downcast(op.into_object()) {
                Ok(stop) => {
                    if let Some(running) = self.runtime.running.remove(&stop.0) {
                        running.abort();
                        OpFlow::Consumed
                    } else {
                        #[cfg(feature = "tokio-mt-task")]
                        {
                            let op: Box<dyn Operation> = Box::new(StopTaskOp(stop.0));
                            OpFlow::Continue(op)
                        }
                        #[cfg(not(feature = "tokio-mt-task"))]
                        {
                            OpFlow::Consumed
                        }
                    }
                }
                Err(err) => OpFlow::Continue(err.into_object()),
            },
        }
    }

    fn drain(&mut self, reconciler: &mut Reconciler) -> bool {
        let mut drained = false;
        while let Some(op) = self.runtime.source.0.borrow_mut().pop_front() {
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

    impl Operation for TestOp {}

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
                let op: Box<dyn Operation> = op;
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
