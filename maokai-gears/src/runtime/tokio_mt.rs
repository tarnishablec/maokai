extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::pin::Pin;
use downcast::Downcast;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Reconciler, Ticket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ops::task::*;

type SendTaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
pub type SendTask = Box<dyn FnOnce(SendTaskEmitter) -> SendTaskFuture + Send + 'static>;
pub type SendTaskMailboxSender =
    mpsc::UnboundedSender<Box<dyn maokai_reconciler::Operation + Send>>;
pub type SendTaskMailboxSource =
    mpsc::UnboundedReceiver<Box<dyn maokai_reconciler::Operation + Send>>;

pub struct SendTaskEmitter {
    sender: SendTaskMailboxSender,
}

impl SendTaskEmitter {
    pub fn new(sender: SendTaskMailboxSender) -> Self {
        Self { sender }
    }

    pub fn into_sender(self) -> SendTaskMailboxSender {
        self.sender
    }

    pub fn emit<Op>(&self, op: Op)
    where
        Op: Operation + Send + 'static,
    {
        let _ = self.sender.send(Box::new(op));
    }
}

fn task_channel() -> (SendTaskMailboxSender, SendTaskMailboxSource) {
    mpsc::unbounded_channel()
}

pub struct TokioMtTaskConsumer {
    running: BTreeMap<TaskHandle, JoinHandle<()>>,
    sender: SendTaskMailboxSender,
    source: SendTaskMailboxSource,
}

impl Default for TokioMtTaskConsumer {
    fn default() -> Self {
        let (sender, source) = task_channel();
        Self {
            running: BTreeMap::new(),
            sender,
            source,
        }
    }
}

impl OpConsumer for TokioMtTaskConsumer {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<TaskOp<SendTask>>::downcast(op) {
            Ok(task_op) => {
                match *task_op {
                    TaskOp::Start { handle, task } => {
                        let running = tokio::spawn(task(SendTaskEmitter::new(self.sender.clone())));
                        self.running.insert(handle, running);
                    }
                    TaskOp::Stop(handle) => {
                        if let Some(running) = self.running.remove(&handle) {
                            running.abort();
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
        while let Ok(op) = self.source.try_recv() {
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
    async fn mt_task_emits_op() {
        let (sender, mut receiver) = task_channel();

        let task: SendTask = Box::new(|emitter| {
            Box::pin(async move {
                emitter.emit(TestOp(42));
            })
        });

        let join = tokio::spawn(task(SendTaskEmitter::new(sender)));
        join.await.unwrap();

        let op = receiver.try_recv().unwrap();
        let op: Box<dyn maokai_reconciler::Operation> = op;
        let value = Downcast::<TestOp>::downcast(op).unwrap();
        assert_eq!(value.0, 42);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn mt_task_abort() {
        let task: SendTask = Box::new(|_| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(999)).await;
            })
        });

        let (sender, mut receiver) = task_channel();
        let join = tokio::spawn(task(SendTaskEmitter::new(sender)));
        join.abort();

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(receiver.try_recv().is_err());
    }
}
