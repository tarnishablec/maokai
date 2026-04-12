use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use core::cell::RefCell;
use downcast::Downcast;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Ticket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(u64);

pub enum TaskOp<T> {
    Start { handle: TaskHandle, task: T },
    Stop(TaskHandle),
}

impl<T: 'static> Operation for TaskOp<T> {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletion<O> {
    pub handle: TaskHandle,
    pub output: O,
}

pub type CompletionSink<O> = Rc<RefCell<VecDeque<TaskCompletion<O>>>>;

pub trait TaskRuntime<T> {
    type Running;
    type Output;

    fn start(
        &mut self,
        handle: TaskHandle,
        task: T,
        sink: CompletionSink<Self::Output>,
    ) -> Self::Running;

    fn stop(&mut self, handle: TaskHandle, running: Self::Running);
}

pub struct TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    runtime: R,
    running: BTreeMap<TaskHandle, R::Running>,
    completions: CompletionSink<R::Output>,
    next_handle: u64,
}

impl<R, T> TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    pub fn new(runtime: R) -> Self {
        Self {
            runtime,
            running: BTreeMap::new(),
            completions: Rc::new(RefCell::new(VecDeque::new())),
            next_handle: 0,
        }
    }

    pub fn next_handle(&mut self) -> TaskHandle {
        let h = TaskHandle(self.next_handle);
        self.next_handle += 1;
        h
    }

    pub fn completions(&self) -> &CompletionSink<R::Output> {
        &self.completions
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
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
                        let running =
                            self.runtime.start(handle, task, self.completions.clone());
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
