extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::any::TypeId;
use core::pin::Pin;
use downcast::Downcast;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Reconciler, Ticket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ops::task::*;

type SendTaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
pub type SendTask = Box<dyn FnOnce(SendTaskEmitter) -> SendTaskFuture + Send + 'static>;
pub type SendTaskMailboxSender = mpsc::UnboundedSender<Box<dyn Operation + Send>>;
pub type SendTaskMailboxSource = mpsc::UnboundedReceiver<Box<dyn Operation + Send>>;

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

struct MtTaskRuntime {
    running: BTreeMap<TaskHandle, JoinHandle<()>>,
    sender: SendTaskMailboxSender,
    source: SendTaskMailboxSource,
}

pub struct TokioMtTaskConsumer {
    runtime: MtTaskRuntime,
}

impl Default for TokioMtTaskConsumer {
    fn default() -> Self {
        let (sender, source) = task_channel();
        Self {
            runtime: MtTaskRuntime {
                running: BTreeMap::new(),
                sender,
                source,
            },
        }
    }
}

impl OpConsumer for TokioMtTaskConsumer {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        let type_id = (*op).type_id();

        if type_id == TypeId::of::<StartTaskOp<SendTask>>() {
            let task_op = Downcast::<StartTaskOp<SendTask>>::downcast(op)
                .expect("type_id matched StartTaskOp<SendTask>");
            let StartTaskOp { handle, task } = *task_op;
            let sender = self.runtime.sender.clone();
            let running = tokio::spawn(task(SendTaskEmitter::new(sender)));
            self.runtime.running.insert(handle, running);
            return OpFlow::Consumed;
        }

        if type_id == TypeId::of::<StopTaskOp>() {
            let stop = Downcast::<StopTaskOp>::downcast(op).expect("type_id matched StopTaskOp");
            if let Some(running) = self.runtime.running.remove(&stop.0) {
                running.abort();
            }
            return OpFlow::Consumed;
        }

        OpFlow::Continue(op)
    }

    fn drain(&mut self, reconciler: &mut Reconciler) -> bool {
        let mut drained = false;
        while let Ok(op) = self.runtime.source.try_recv() {
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
        let op: Box<dyn Operation> = op;
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
