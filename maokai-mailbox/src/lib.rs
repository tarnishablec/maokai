#![no_std]
extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use maokai_runner::{Behaviors, Runner};
use maokai_task::{TaskHandle, TaskOp, WithTask};
use maokai_tree::{State, StateTree, TreeView};

pub mod runtimes;

pub trait MailboxRuntime<E: 'static, S> {
    type Running;

    fn start(&mut self, handle: TaskHandle, task: S) -> Self::Running;

    fn stop(&mut self, running: Self::Running);

    fn poll_completed(&mut self) -> Option<(TaskHandle, E)>;
}

pub struct Mailbox<'a, T, E: 'static, C, R, S>
where
    R: MailboxRuntime<E, S>,
{
    runner: Runner<'a, T>,
    behaviors: &'a Behaviors<'a, E, WithTask<C, S>>,
    current: State,
    context: WithTask<C, S>,
    queue: VecDeque<E>,
    runtime: R,
    running: BTreeMap<TaskHandle, R::Running>,
}

impl<'a, T, E: 'static, C, R, S> Mailbox<'a, T, E, C, R, S>
where
    StateTree<T>: TreeView,
    R: MailboxRuntime<E, S>,
{
    pub fn new(
        tree: &'a StateTree<T>,
        behaviors: &'a Behaviors<'a, E, WithTask<C, S>>,
        initial: State,
        context: WithTask<C, S>,
        runtime: R,
    ) -> Self {
        Self {
            runner: Runner::new(tree),
            behaviors,
            current: initial,
            context,
            queue: VecDeque::new(),
            runtime,
            running: BTreeMap::new(),
        }
    }

    pub fn post(&mut self, event: E) {
        self.queue.push_back(event);
    }

    pub fn current(&self) -> State {
        self.current
    }

    pub fn context(&self) -> &C {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut C {
        &mut self.context
    }

    pub fn step(&mut self) -> bool {
        let mut progressed = false;

        if let Some((handle, event)) = self.runtime.poll_completed()
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

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use async_trait::async_trait;
    use core::future::Future;
    use core::pin::Pin;
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_task::Task;
    use std::sync::Arc;
    use std::task::{Context as TaskContext, Poll, Wake, Waker};

    type LocalTaskBox<E> = Box<dyn Task<Event = E>>;

    #[derive(Debug)]
    enum Event {
        Begin,
        Done,
    }

    struct EventTask;

    #[async_trait]
    impl Task for EventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            Event::Done
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct Ctx {
        active_task: Option<TaskHandle>,
    }

    struct IdleBehavior {
        loading: State,
    }

    struct LoadingBehavior {
        idle: State,
    }

    impl<C> Behavior<Event, WithTask<C, LocalTaskBox<Event>>> for IdleBehavior {
        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut WithTask<C, LocalTaskBox<Event>>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                Event::Begin => EventReply::Transition(self.loading),
                Event::Done => EventReply::Ignored,
            }
        }
    }

    impl<C> Behavior<Event, WithTask<C, LocalTaskBox<Event>>> for LoadingBehavior
    where
        C: core::borrow::BorrowMut<Ctx>,
    {
        fn on_enter(
            &self,
            _transition: &Transition,
            context: &mut WithTask<C, LocalTaskBox<Event>>,
        ) {
            let handle = context
                .reconciler()
                .start(Box::new(EventTask) as LocalTaskBox<Event>);
            let ctx: &mut Ctx = core::borrow::BorrowMut::borrow_mut(&mut **context);
            ctx.active_task = Some(handle);
        }

        fn on_exit(
            &self,
            _transition: &Transition,
            context: &mut WithTask<C, LocalTaskBox<Event>>,
        ) {
            let handle = {
                let ctx: &mut Ctx = core::borrow::BorrowMut::borrow_mut(&mut **context);
                ctx.active_task.take()
            };

            if let Some(handle) = handle {
                context.reconciler().stop(handle);
            }
        }

        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut WithTask<C, LocalTaskBox<Event>>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                Event::Done => EventReply::Transition(self.idle),
                Event::Begin => EventReply::Ignored,
            }
        }
    }

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker: Waker = Waker::from(Arc::new(NoopWake));
        let mut context = TaskContext::from_waker(&waker);
        let mut future = Box::pin(future);

        loop {
            match Pin::as_mut(&mut future).poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    struct InlineRuntime<E: 'static> {
        completed: VecDeque<(TaskHandle, E)>,
        stopped: Vec<TaskHandle>,
    }

    impl<E: 'static> Default for InlineRuntime<E> {
        fn default() -> Self {
            Self {
                completed: VecDeque::new(),
                stopped: Vec::new(),
            }
        }
    }

    impl<E: 'static> MailboxRuntime<E, LocalTaskBox<E>> for InlineRuntime<E> {
        type Running = TaskHandle;

        fn start(&mut self, handle: TaskHandle, task: LocalTaskBox<E>) -> Self::Running {
            let event = block_on(task.run());
            self.completed.push_back((handle, event));
            handle
        }

        fn stop(&mut self, running: Self::Running) {
            self.stopped.push(running);
        }

        fn poll_completed(&mut self) -> Option<(TaskHandle, E)> {
            self.completed.pop_front()
        }
    }

    fn build_tree() -> (StateTree<&'static str>, State, State) {
        let mut tree = StateTree::new("root");
        let idle = tree.add_child(&tree.root(), "idle");
        let loading = tree.add_child(&tree.root(), "loading");
        (tree, idle, loading)
    }

    #[test]
    fn mailbox_runs_tasks_and_feeds_events_back() {
        let (tree, idle, loading) = build_tree();

        let mut behaviors = Behaviors::default();
        behaviors.register(&idle, IdleBehavior { loading });
        behaviors.register(&loading, LoadingBehavior { idle });

        let runtime = InlineRuntime::default();
        let mut mailbox = Mailbox::new(
            &tree,
            &behaviors,
            idle,
            WithTask::<_, LocalTaskBox<Event>>::new(Ctx::default()),
            runtime,
        );

        mailbox.post(Event::Begin);
        mailbox.run_until_stable();

        assert_eq!(mailbox.current(), idle);
        assert_eq!(mailbox.context().active_task, None);
        assert_eq!(mailbox.runtime.stopped.len(), 1);
    }
}
