#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use maokai_runner::{Behaviors, Runner};
use maokai_task::{Task, TaskHandle, TaskOp, WithTask};
use maokai_tree::{State, StateTree, TreeView};

pub mod runtimes;

pub trait MailboxRuntime<E: 'static> {
    type Running;
    type Task: ?Sized + 'static;

    fn start(&mut self, handle: TaskHandle, task: Box<Self::Task>) -> Self::Running;

    fn stop(&mut self, running: Self::Running);

    fn poll(&mut self) -> Option<(TaskHandle, E)>;
}

pub struct Mailbox<'a, T, E: 'static, C, R>
where
    R: MailboxRuntime<E>,
{
    runner: Runner<'a, T>,
    behaviors: &'a Behaviors<'a, E, WithTask<C, E>>,
    current: State,
    context: WithTask<C, E>,
    queue: VecDeque<E>,
    runtime: R,
    running: BTreeMap<TaskHandle, R::Running>,
}

impl<'a, Tree, Event: 'static, Context, Runtime> Mailbox<'a, Tree, Event, Context, Runtime>
where
    StateTree<Tree>: TreeView,
    Runtime: MailboxRuntime<Event, Task = dyn Task<Event = Event>>,
{
    pub fn new(
        tree: &'a StateTree<Tree>,
        behaviors: &'a Behaviors<'a, Event, WithTask<Context, Event>>,
        initial: State,
        context: Context,
        runtime: Runtime,
    ) -> Self {
        Self {
            runner: Runner::new(tree),
            behaviors,
            current: initial,
            context: WithTask::new(context),
            queue: VecDeque::new(),
            runtime,
            running: BTreeMap::new(),
        }
    }

    pub fn post(&mut self, event: Event) {
        self.queue.push_back(event);
    }

    pub fn current(&self) -> State {
        self.current
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut Context {
        &mut self.context
    }

    pub fn step(&mut self) -> bool {
        let mut progressed = false;

        if let Some((handle, event)) = self.runtime.poll()
            && self.running.contains_key(&handle)
        {
            self.queue.push_back(event);
            progressed = true;
        }

        if let Some(event) = self.queue.pop_front() {
            self.current =
                self.runner
                    .dispatch(self.behaviors, &self.current, &event, &mut self.context);
            progressed = true;

            for op in self.context.reconciler().drain() {
                match op {
                    TaskOp::Start { handle, task } => {
                        let running = self.runtime.start(handle, task);
                        self.running.insert(handle, running);
                    }
                    TaskOp::Stop { handle } => {
                        if let Some(running) = self.running.remove(&handle) {
                            self.runtime.stop(running);
                        }
                    }
                }
            }
        }

        progressed
    }

    pub fn run_until_stable(&mut self) {
        while self.step() {}
    }
}
