use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::TaskRuntime;

pub type LocalTaskBox<E> = Box<dyn Task<Event = E>>;

pub struct TokioLocalRuntime<E: 'static> {
    tx: ::tokio::sync::mpsc::UnboundedSender<(TaskHandle, E)>,
    rx: ::tokio::sync::mpsc::UnboundedReceiver<(TaskHandle, E)>,
}

impl<E: 'static> TokioLocalRuntime<E> {
    pub fn new() -> Self {
        let (tx, rx) = ::tokio::sync::mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

impl<E: 'static> Default for TokioLocalRuntime<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: 'static> TaskRuntime<E, LocalTaskBox<E>> for TokioLocalRuntime<E> {
    type Running = ::tokio::task::JoinHandle<()>;

    fn start(&mut self, handle: TaskHandle, task: LocalTaskBox<E>) -> Self::Running {
        let tx = self.tx.clone();

        // `Task` is not required to be `Send`, so the local Tokio runtime uses `spawn_local`.
        ::tokio::task::spawn_local(async move {
            let event = task.run().await;
            let _ = tx.send((handle, event));
        })
    }

    fn stop(&mut self, running: Self::Running) {
        running.abort();
    }

    fn poll_completed(&mut self) -> Option<(TaskHandle, E)> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::marker::PhantomData;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::LazyLock;

    use async_trait::async_trait;
    use maokai_runner::{Behavior, Behaviors, EventReply, Transition};
    use maokai_task::{Task, TaskHandle, WithTask};
    use maokai_tree::{State, StateTree, TreeView};

    use super::*;
    use crate::Mailbox;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        Begin,
        Done,
    }

    static SEND_TASK_RUNS: AtomicUsize = AtomicUsize::new(0);
    static LOCAL_TASK_RUNS: AtomicUsize = AtomicUsize::new(0);

    static INSTANCE_TREE: LazyLock<(StateTree<&'static str>, State, State)> = LazyLock::new(|| {
        let mut tree = StateTree::new("root");
        let idle = tree.add_child(&tree.root(), "idle");
        let loading = tree.add_child(&tree.root(), "loading");
        (tree, idle, loading)
    });

    #[derive(Default)]
    struct Ctx {
        active_task: Option<TaskHandle>,
    }

    struct IdleBehavior {
        loading: State,
    }

    struct LoadingBehavior<F, T> {
        idle: State,
        make_task: F,
        marker: PhantomData<fn() -> T>,
    }

    struct SendEventTask;
    struct LocalEventTask {
        marker: PhantomData<*const ()>,
    }

    unsafe impl Sync for LocalEventTask {}

    #[async_trait]
    impl Task for SendEventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            SEND_TASK_RUNS.fetch_add(1, Ordering::SeqCst);
            Event::Done
        }
    }

    #[async_trait]
    impl Task for LocalEventTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            LOCAL_TASK_RUNS.fetch_add(1, Ordering::SeqCst);
            Event::Done
        }
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

    impl<C, F, T> Behavior<Event, WithTask<C, LocalTaskBox<Event>>> for LoadingBehavior<F, T>
    where
        C: core::borrow::BorrowMut<Ctx>,
        F: Fn() -> T + Send + Sync + 'static,
        T: Task<Event = Event> + 'static,
    {
        fn on_enter(
            &self,
            _transition: &Transition,
            context: &mut WithTask<C, LocalTaskBox<Event>>,
        ) {
            let handle = context
                .reconciler()
                .start(Box::new((self.make_task)()) as LocalTaskBox<Event>);
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

    fn make_send_task() -> SendEventTask {
        SendEventTask
    }

    fn make_local_task() -> LocalEventTask {
        LocalEventTask {
            marker: PhantomData,
        }
    }

    fn build_behaviors<F, T>(
        idle: State,
        loading: State,
        make_task: F,
    ) -> Behaviors<'static, Event, WithTask<Ctx, LocalTaskBox<Event>>>
    where
        F: Fn() -> T + Send + Sync + 'static,
        T: Task<Event = Event> + 'static,
    {
        let mut behaviors = Behaviors::default();
        behaviors.register(&idle, IdleBehavior { loading });
        behaviors.register(
            &loading,
            LoadingBehavior {
                idle,
                make_task,
                marker: PhantomData,
            },
        );
        behaviors
    }

    async fn run_mailbox_case<F, T>(make_task: F)
    where
        F: Fn() -> T + Send + Sync + 'static,
        T: Task<Event = Event> + 'static,
    {
        let (tree, idle, loading) = &*INSTANCE_TREE;
        let behaviors = build_behaviors(*idle, *loading, make_task);
        let mut mailbox = Mailbox::new(
            tree,
            &behaviors,
            *idle,
            WithTask::<_, LocalTaskBox<Event>>::new(Ctx::default()),
            TokioLocalRuntime::new(),
        );

        mailbox.post(Event::Begin);

        for _ in 0..8 {
            if !mailbox.step() {
                ::tokio::task::yield_now().await;
            }
        }

        assert_eq!(mailbox.current(), *idle);
        assert_eq!(mailbox.context().active_task, None);
    }

    #[test]
    fn tokio_local_runtime_runs_send_task_mailbox_case() {
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime should build");
        let local = ::tokio::task::LocalSet::new();

        SEND_TASK_RUNS.store(0, Ordering::SeqCst);
        local.block_on(&runtime, async {
            run_mailbox_case(make_send_task).await;
        });

        assert_eq!(SEND_TASK_RUNS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tokio_local_runtime_runs_non_send_task_mailbox_case() {
        let runtime = ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime should build");
        let local = ::tokio::task::LocalSet::new();

        LOCAL_TASK_RUNS.store(0, Ordering::SeqCst);
        local.block_on(&runtime, async {
            run_mailbox_case(make_local_task).await;
        });

        assert_eq!(LOCAL_TASK_RUNS.load(Ordering::SeqCst), 1);
    }
}
