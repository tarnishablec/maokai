#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::ops::{Deref, DerefMut};

#[async_trait]
pub trait Task: 'static {
    type Event;
    async fn run(&self) -> Self::Event;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(u64);

pub enum TaskOp<E: 'static> {
    Start {
        handle: TaskHandle,
        task: Box<dyn Task<Event = E>>,
    },
    Stop {
        handle: TaskHandle,
    },
}

pub struct Reconciler<E: 'static> {
    next_handle: u64,
    pending: Vec<TaskOp<E>>,
}

impl<E: 'static> Default for Reconciler<E> {
    fn default() -> Self {
        Self {
            next_handle: 0,
            pending: Vec::new(),
        }
    }
}

impl<E: 'static> Reconciler<E> {
    pub fn start<T>(&mut self, task: T) -> TaskHandle
    where
        T: Task<Event = E>,
    {
        let handle = TaskHandle(self.next_handle);
        self.next_handle += 1;
        self.pending.push(TaskOp::Start {
            handle,
            task: Box::new(task),
        });
        handle
    }

    pub fn stop(&mut self, handle: TaskHandle) {
        self.pending.push(TaskOp::Stop { handle });
    }

    pub fn drain(&mut self) -> Vec<TaskOp<E>> {
        core::mem::take(&mut self.pending)
    }
}

pub struct WithTask<E: 'static, C> {
    context: C,
    reconciler: Reconciler<E>,
}

impl<E: 'static, C> WithTask<E, C> {
    pub fn new(context: C) -> Self {
        Self {
            context,
            reconciler: Reconciler::default(),
        }
    }

    pub fn reconciler(&mut self) -> &mut Reconciler<E> {
        &mut self.reconciler
    }
}

impl<E: 'static, C> Deref for WithTask<E, C> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl<E: 'static, C> DerefMut for WithTask<E, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.context
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use maokai_runner::{Behavior, Behaviors, EventReply, Runner, Transition};
    use maokai_tree::{State, StateTree, TreeView};

    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context as TaskContext, Poll, Wake, Waker};

    struct DummyTask(u32);

    struct EventTask;

    #[derive(Debug)]
    enum Event {
        Begin,
        Cancel,
        Done,
    }

    #[async_trait]
    impl Task for DummyTask {
        type Event = u32;

        async fn run(&self) -> Self::Event {
            self.0
        }
    }

    #[async_trait]
    impl Task for EventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            Event::Done
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct Ctx {
        count: u32,
        active_task: Option<TaskHandle>,
    }

    struct IdleBehavior {
        loading: State,
    }

    struct LoadingBehavior {
        idle: State,
    }

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    fn build_task_tree() -> (StateTree<&'static str>, State, State) {
        let mut tree = StateTree::new("root");
        let idle = tree.add_child(&tree.root(), "idle");
        let loading = tree.add_child(&tree.root(), "loading");
        (tree, idle, loading)
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

    impl<C> Behavior<Event, WithTask<Event, C>> for IdleBehavior {
        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut WithTask<Event, C>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                Event::Begin => EventReply::Transition(self.loading),
                _ => EventReply::Ignored,
            }
        }
    }

    impl<C> Behavior<Event, WithTask<Event, C>> for LoadingBehavior
    where
        C: core::borrow::BorrowMut<Ctx>,
    {
        fn on_enter(&self, _transition: &Transition, context: &mut WithTask<Event, C>) {
            let handle = context.reconciler().start(EventTask);
            let ctx: &mut Ctx = core::borrow::BorrowMut::borrow_mut(&mut context.context);
            ctx.active_task = Some(handle);
        }

        fn on_exit(&self, _transition: &Transition, context: &mut WithTask<Event, C>) {
            let handle = {
                let ctx: &mut Ctx = core::borrow::BorrowMut::borrow_mut(&mut context.context);
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
            _context: &mut WithTask<Event, C>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                Event::Cancel | Event::Done => EventReply::Transition(self.idle),
                _ => EventReply::Ignored,
            }
        }
    }

    #[test]
    fn start_returns_incrementing_handles() {
        let mut reconciler = Reconciler::<u32>::default();

        let first = reconciler.start(DummyTask(1));
        let second = reconciler.start(DummyTask(2));

        assert_ne!(first, second);
        match reconciler.drain().as_slice() {
            [
                TaskOp::Start { handle: h1, .. },
                TaskOp::Start { handle: h2, .. },
            ] => {
                assert_eq!((*h1, *h2), (first, second));
            }
            other => panic!("unexpected ops: {}", other.len()),
        }
    }

    #[test]
    fn stop_is_recorded_and_drain_clears_pending() {
        let mut reconciler = Reconciler::<u32>::default();
        let handle = reconciler.start(DummyTask(7));

        reconciler.stop(handle);

        match reconciler.drain().as_slice() {
            [
                TaskOp::Start {
                    handle: started, ..
                },
                TaskOp::Stop { handle: stopped },
            ] => {
                assert_eq!((*started, *stopped), (handle, handle));
            }
            other => panic!("unexpected ops: {}", other.len()),
        }
        assert!(reconciler.drain().is_empty());
    }

    #[test]
    fn with_task_exposes_context_and_reconciler() {
        let mut with_task = WithTask::<u32, _>::new(Ctx::default());

        with_task.count += 1;
        let handle = with_task.reconciler().start(DummyTask(3));

        assert_eq!(with_task.count, 1);
        match with_task.reconciler().drain().as_slice() {
            [
                TaskOp::Start {
                    handle: started, ..
                },
            ] => assert_eq!(*started, handle),
            other => panic!("unexpected ops: {}", other.len()),
        }
    }

    #[test]
    fn tree_runner_and_task_reconciler_work_together() {
        let (tree, idle, loading) = build_task_tree();

        let mut behaviors = Behaviors::default();
        behaviors.register(&idle, IdleBehavior { loading });
        behaviors.register(&loading, LoadingBehavior { idle });

        let runner = Runner::new(&tree);
        let mut context = WithTask::<Event, _>::new(Ctx::default());

        let current = runner.dispatch(&behaviors, &idle, &Event::Begin, &mut context);
        assert_eq!(current, loading);

        let started_handle = match context.active_task {
            Some(handle) => handle,
            None => panic!("task should be active after entering loading"),
        };

        match context.reconciler().drain().as_slice() {
            [TaskOp::Start { handle, .. }] => assert_eq!(*handle, started_handle),
            other => panic!("unexpected start ops: {}", other.len()),
        }

        let current = runner.dispatch(&behaviors, &current, &Event::Cancel, &mut context);
        assert_eq!(current, idle);
        assert_eq!(context.active_task, None);

        match context.reconciler().drain().as_slice() {
            [TaskOp::Stop { handle }] => assert_eq!(*handle, started_handle),
            other => panic!("unexpected stop ops: {}", other.len()),
        }
    }

    #[test]
    fn runtime_loop_runs_tasks_and_feeds_events_back() {
        let (tree, idle, loading) = build_task_tree();

        let mut ctx = Ctx::default();
        let mut context = WithTask::<Event, _>::new(&mut ctx);

        let mut behaviors = Behaviors::default();
        behaviors.register(&idle, IdleBehavior { loading });
        behaviors.register(&loading, LoadingBehavior { idle });

        let runner = Runner::new(&tree);
        let mut current = idle;
        let mut queue = VecDeque::from([Event::Begin]);
        let mut started = Vec::new();
        let mut stopped = Vec::new();

        while let Some(event) = queue.pop_front() {
            current = runner.dispatch(&behaviors, &current, &event, &mut context);

            for op in context.reconciler().drain() {
                match op {
                    TaskOp::Start { handle, task } => {
                        started.push(handle);
                        let event = block_on(task.run());
                        queue.push_back(event);
                    }
                    TaskOp::Stop { handle } => stopped.push(handle),
                }
            }
        }

        assert_eq!(current, idle);
        assert_eq!(context.active_task, None);
        assert_eq!(started.len(), 1);
        assert_eq!(stopped, started);
    }
}
