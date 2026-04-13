use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use core::cell::RefCell;
use downcast::Downcast;
use maokai_reconciler::{HasReconciler, OpConsumer, OpFlow, Operation, Ticket};
use slotmap::{SlotMap, new_key_type};

new_key_type! {
    pub struct TaskHandle;
}

pub struct TaskHandles {
    handles: SlotMap<TaskHandle, ()>,
}

impl Default for TaskHandles {
    fn default() -> Self {
        Self {
            handles: SlotMap::with_key(),
        }
    }
}

impl TaskHandles {
    pub fn alloc(&mut self) -> TaskHandle {
        self.handles.insert(())
    }
}

pub trait TaskSpawner {
    fn alloc_task_handle(&mut self) -> TaskHandle;
}

pub enum TaskOp<T> {
    Start { handle: TaskHandle, task: T },
    Stop(TaskHandle),
}

impl<T: 'static> Operation for TaskOp<T> {}

pub trait TaskOpsExt<T: 'static>: HasReconciler + TaskSpawner {
    fn start_task(&mut self, task: T) -> Option<TaskHandle> {
        let handle = self.alloc_task_handle();
        self.reconciler()
            .stage(TaskOp::Start { handle, task }, None)
            .map(|_| handle)
    }

    fn stop_task(&mut self, handle: TaskHandle) -> Option<Ticket> {
        self.reconciler().stage(TaskOp::<T>::Stop(handle), None)
    }
}

impl<T: 'static, Ctx> TaskOpsExt<T> for Ctx where Ctx: HasReconciler + TaskSpawner {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletion<O> {
    pub handle: TaskHandle,
    pub output: O,
}

// --- Completion channel abstraction ---

pub trait CompletionSender<O>: Clone {
    fn send(&self, completion: TaskCompletion<O>);
}

pub trait CompletionReceiver<O> {
    fn try_recv(&mut self) -> Option<TaskCompletion<O>>;
}

// Local channel: Rc<RefCell<VecDeque>> serves as both sender and receiver.

impl<O> CompletionSender<O> for Rc<RefCell<VecDeque<TaskCompletion<O>>>> {
    fn send(&self, completion: TaskCompletion<O>) {
        self.borrow_mut().push_back(completion);
    }
}

impl<O> CompletionReceiver<O> for Rc<RefCell<VecDeque<TaskCompletion<O>>>> {
    fn try_recv(&mut self) -> Option<TaskCompletion<O>> {
        self.borrow_mut().pop_front()
    }
}

// --- TaskRuntime ---

pub trait TaskRuntime<T> {
    type Running;
    type Output;
    type Sender: CompletionSender<Self::Output>;
    type Receiver: CompletionReceiver<Self::Output>;

    fn create_channel(&self) -> (Self::Sender, Self::Receiver);
    fn start(&mut self, handle: TaskHandle, task: T, sender: Self::Sender) -> Self::Running;
    fn stop(&mut self, handle: TaskHandle, running: Self::Running);
}

// --- TaskOpConsumer ---

pub struct TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    runtime: R,
    running: BTreeMap<TaskHandle, R::Running>,
    sender: R::Sender,
}

impl<R, T> TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    pub fn new(runtime: R) -> (Self, R::Receiver) {
        let (sender, receiver) = runtime.create_channel();
        let consumer = Self {
            runtime,
            running: BTreeMap::new(),
            sender,
        };
        (consumer, receiver)
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
                        let running = self.runtime.start(handle, task, self.sender.clone());
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
