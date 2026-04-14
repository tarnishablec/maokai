use core::sync::atomic::{AtomicU64, Ordering};
use maokai_reconciler::{HasReconciler, Operation, Ticket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(u64);

impl TaskHandle {
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct StartTaskOp<T> {
    pub handle: TaskHandle,
    pub task: T,
}

impl<T: 'static> Operation for StartTaskOp<T> {}

pub struct StopTaskOp(pub TaskHandle);

impl Operation for StopTaskOp {}

pub trait TaskOpsExt<T: 'static>: HasReconciler {
    fn start_task(&mut self, task: T) -> Option<TaskHandle> {
        let handle = TaskHandle::next();
        self.reconciler()
            .stage(StartTaskOp { handle, task }, None)
            .map(|_| handle)
    }

    fn stop_task(&mut self, handle: TaskHandle) -> Option<Ticket> {
        self.reconciler().stage(StopTaskOp(handle), None)
    }
}

impl<T: 'static, Ctx> TaskOpsExt<T> for Ctx where Ctx: HasReconciler {}
