pub mod tokio;

#[cfg(all(test, any(feature = "tokio-local", feature = "tokio-mt")))]
#[allow(dead_code)]
pub(crate) mod test_support {
    use alloc::boxed::Box;
    extern crate std;

    use core::marker::PhantomData;
    use std::sync::LazyLock;

    use maokai_runner::{Behavior, EventReply};
    use maokai_task::{Task, TaskHandle, TaskContext};
    use maokai_tree::{State, StateTree, TreeView};

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Event {
        Begin,
        Done,
        Cancel,
    }

    pub(crate) static STATE_TREE: LazyLock<(StateTree<&'static str>, State, State)> =
        LazyLock::new(|| {
            let mut tree = StateTree::new("root");
            let idle = tree.add_child(&tree.root(), "idle");
            let loading = tree.add_child(&tree.root(), "loading");
            (tree, idle, loading)
        });

    #[derive(Default)]
    pub(crate) struct Ctx {
        pub(crate) active_task: Option<TaskHandle>,
    }

    pub(crate) struct IdleBehavior {}

    pub(crate) struct LoadingBehavior<F, W, T: ?Sized, S> {
        pub(crate) make_task: F,
        pub(crate) wrap_task: W,
        pub(crate) marker: PhantomData<fn() -> (*const T, S)>,
    }

    impl<C, T> Behavior<Event, TaskContext<C, Event, T>> for IdleBehavior
    where
        T: Task<Event = Event> + ?Sized + 'static,
    {
        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut TaskContext<C, Event, T>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, _, loading) = &*STATE_TREE;
            match event {
                Event::Begin => EventReply::Transition(*loading),
                Event::Done | Event::Cancel => EventReply::Ignored,
            }
        }
    }

    impl<F, W, T, S> Behavior<Event, TaskContext<Ctx, Event, T>> for LoadingBehavior<F, W, T, S>
    where
        F: Fn() -> S + Send + Sync,
        W: Fn(S) -> Box<T> + Send + Sync,
        T: Task<Event = Event> + ?Sized + 'static,
        S: Task<Event = Event> + 'static,
    {
        fn on_enter(
            &self,
            _transition: &maokai_runner::Transition,
            context: &mut TaskContext<Ctx, Event, T>,
        ) {
            let handle = context
                .reconciler()
                .start((self.wrap_task)((self.make_task)()));
            context.active_task = Some(handle);
        }

        fn on_exit(
            &self,
            _transition: &maokai_runner::Transition,
            context: &mut TaskContext<Ctx, Event, T>,
        ) {
            if let Some(handle) = context.active_task.take() {
                context.reconciler().stop(handle);
            }
        }

        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut TaskContext<Ctx, Event, T>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, idle, _) = &*STATE_TREE;
            match event {
                Event::Done | Event::Cancel => EventReply::Transition(*idle),
                Event::Begin => EventReply::Ignored,
            }
        }
    }
}
