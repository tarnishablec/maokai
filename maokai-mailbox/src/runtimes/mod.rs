pub mod tokio;

#[cfg(all(test, any(feature = "tokio-local", feature = "tokio-mt")))]
pub(crate) mod test_support {
    extern crate std;

    use core::marker::PhantomData;
    use std::sync::LazyLock;

    use maokai_runner::{Behavior, EventReply};
    use maokai_task::{TaskHandle, WithTask};
    use maokai_tree::{State, StateTree, TreeView};

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Event {
        Begin,
        Done,
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

    pub(crate) struct LoadingBehavior<F, W, T, S> {
        pub(crate) make_task: F,
        pub(crate) wrap_task: W,
        pub(crate) marker: PhantomData<fn() -> (T, S)>,
    }

    impl<C> Behavior<Event, WithTask<C, Event>> for IdleBehavior {
        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut WithTask<C, Event>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, _, loading) = &*STATE_TREE;
            match event {
                Event::Begin => EventReply::Transition(*loading),
                Event::Done => EventReply::Ignored,
            }
        }
    }
}
