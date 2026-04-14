use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU64, Ordering};
use downcast::Downcast;
use maokai_reconciler::{HasReconciler, OpConsumer, OpFlow, Operation, Reconciler, Ticket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(u64);

impl TaskHandle {
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

pub enum TaskOp<T> {
    Start { handle: TaskHandle, task: T },
    Stop(TaskHandle),
}

impl<T: 'static> Operation for TaskOp<T> {}

pub trait TaskOpsExt<T: 'static>: HasReconciler {
    fn start_task(&mut self, task: T) -> Option<TaskHandle> {
        let handle = TaskHandle::next();
        self.reconciler()
            .stage(TaskOp::Start { handle, task }, None)
            .map(|_| handle)
    }

    fn stop_task(&mut self, handle: TaskHandle) -> Option<Ticket> {
        self.reconciler().stage(TaskOp::<T>::Stop(handle), None)
    }
}

impl<T: 'static, Ctx> TaskOpsExt<T> for Ctx where Ctx: HasReconciler {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletion<O> {
    pub handle: TaskHandle,
    pub output: O,
}

pub enum TaskOutput<O> {
    Operation(Box<dyn Operation + Send>),
    Completion(TaskCompletion<O>),
}

pub struct TaskCompletionOp<O>(pub TaskCompletion<O>);

impl<O: 'static> Operation for TaskCompletionOp<O> {}

pub struct TaskEmitter<S, O> {
    sender: S,
    _marker: PhantomData<fn() -> O>,
}

impl<S, O> TaskEmitter<S, O> {
    pub fn new(sender: S) -> Self {
        Self {
            sender,
            _marker: PhantomData,
        }
    }

    pub fn into_sender(self) -> S {
        self.sender
    }
}

impl<S: Clone, O> Clone for TaskEmitter<S, O> {
    fn clone(&self) -> Self {
        Self::new(self.sender.clone())
    }
}

impl<S, O> TaskEmitter<S, O>
where
    S: TaskMailboxSender<O>,
{
    pub fn emit<Op>(&self, op: Op)
    where
        Op: Operation + Send + 'static,
    {
        self.sender.send_op(Box::new(op));
    }
}

// --- Task mailbox abstraction ---

pub trait TaskMailboxSender<O>: Clone {
    fn send_op(&self, op: Box<dyn Operation + Send>);
    fn send_completion(&self, completion: TaskCompletion<O>);
}

pub trait TaskMailboxSource<O> {
    fn try_recv(&mut self) -> Option<TaskOutput<O>>;
}

pub struct TaskCompletionConsumer<O, F> {
    map: F,
    _marker: PhantomData<fn(O)>,
}

impl<O, F> TaskCompletionConsumer<O, F> {
    pub fn new(map: F) -> Self {
        Self {
            map,
            _marker: PhantomData,
        }
    }
}

impl<O: 'static, F> OpConsumer for TaskCompletionConsumer<O, F>
where
    F: FnMut(TaskCompletion<O>) -> OpFlow,
{
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<TaskCompletionOp<O>>::downcast(op) {
            Ok(completion) => (self.map)(completion.0),
            Err(err) => OpFlow::Continue(err.into_object()),
        }
    }
}

// --- TaskRuntime ---

pub trait TaskRuntime<T> {
    type Running;
    type Output;

    type Sender: TaskMailboxSender<Self::Output>;
    type Source: TaskMailboxSource<Self::Output> + 'static;

    fn create_channel(&self) -> (Self::Sender, Self::Source);
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
    source: Box<dyn TaskMailboxSource<R::Output>>,
}

impl<R, T> TaskOpConsumer<R, T>
where
    R: TaskRuntime<T>,
{
    pub fn new(runtime: R) -> Self {
        let (sender, source) = runtime.create_channel();
        Self {
            runtime,
            running: BTreeMap::new(),
            sender,
            source: Box::new(source),
        }
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
    R::Output: 'static,
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

    fn drain(&mut self, reconciler: &mut Reconciler) -> bool {
        let mut drained = false;
        while let Some(output) = self.source.try_recv() {
            drained = true;
            match output {
                TaskOutput::Operation(op) => {
                    let op: Box<dyn Operation> = op;
                    let _ = reconciler.stage_boxed(op, None);
                }
                TaskOutput::Completion(completion) => {
                    let _ = reconciler.stage(TaskCompletionOp(completion), None);
                }
            }
        }
        drained
    }
}
