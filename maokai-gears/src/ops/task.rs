use alloc::boxed::Box;
use downcast::Downcast;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Ticket};
use alloc::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(pub u64);

pub enum TaskOp<T> {
    Start { handle: TaskHandle, task: T },
    Stop(TaskHandle),
}

impl<T: 'static> Operation for TaskOp<T> {}

//

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletion<O> {
    pub handle: TaskHandle,
    pub output: O,
}

pub trait TaskRuntime<T> {
    type Running;
    type Output;

    fn start(&mut self, handle: TaskHandle, task: T) -> Self::Running;
    fn stop(&mut self, handle: TaskHandle, running: Self::Running);
    fn poll(&mut self) -> Option<TaskCompletion<Self::Output>>;
}

pub struct TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    runtime: R,
    running: BTreeMap<TaskHandle, R::Running>,
}

impl<R, T> TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    pub fn new(runtime: R) -> Self {
        Self {
            runtime,
            running: BTreeMap::new(),
        }
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }

    pub fn poll(&mut self) -> Option<TaskCompletion<R::Output>> {
        let completion = self.runtime.poll()?;
        self.running.remove(&completion.handle);
        Some(completion)
    }
}

impl<R, T> OpConsumer for TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
    T: 'static,
{
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<TaskOp<T>>::downcast(op) {
            Ok(task_op) => {
                match *task_op {
                    TaskOp::Start { handle, task } => {
                        let running = self.runtime.start(handle, task);
                        self.running.insert(handle, running);
                    }
                    TaskOp::Stop(handle) => {
                        if let Some(running) = self.running.remove(&handle) {
                            self.runtime.stop(handle, running);
                        }
                    }
                }
                OpFlow::Consumed
            }
            Err(err) => OpFlow::Continue(err.into_object()),
        }
    }
}
