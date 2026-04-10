pub mod tokio;

#[cfg(all(test, any(feature = "tokio-local", feature = "tokio-mt")))]
pub(crate) mod test_support {
    extern crate std;

    use core::marker::PhantomData;
    use std::sync::LazyLock;

    use maokai_runner::{Behavior, Behaviors, EventReply, Transition};
    use maokai_task::{TaskHandle, WithTask};
    use maokai_tree::{State, StateTree, TreeView};

    use crate::{Mailbox, MailboxRuntime};

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Event {
        Begin,
        Done,
    }

    pub(crate) static INSTANCE_TREE: LazyLock<(StateTree<&'static str>, State, State)> =
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

    pub(crate) struct IdleBehavior {
        pub(crate) loading: State,
    }

    pub(crate) struct LoadingBehavior<F, W, T, S> {
        pub(crate) idle: State,
        pub(crate) make_task: F,
        pub(crate) wrap_task: W,
        pub(crate) marker: PhantomData<fn() -> (T, S)>,
    }

    impl<C, S> Behavior<Event, WithTask<C, S>> for IdleBehavior {
        fn on_event(
            &self,
            event: &Event,
            _current: &State,
            _context: &mut WithTask<C, S>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                Event::Begin => EventReply::Transition(self.loading),
                Event::Done => EventReply::Ignored,
            }
        }
    }

    impl<C, S, F, W, T> Behavior<Event, WithTask<C, S>> for LoadingBehavior<F, W, T, S>
    where
        C: core::borrow::BorrowMut<Ctx>,
        F: Fn() -> T + Send + Sync + 'static,
        W: Fn(T) -> S + Send + Sync + 'static,
    {
        fn on_enter(&self, _transition: &Transition, context: &mut WithTask<C, S>) {
            let handle = context
                .reconciler()
                .start((self.wrap_task)((self.make_task)()));
            let ctx = core::borrow::BorrowMut::borrow_mut(&mut **context);
            ctx.active_task = Some(handle);
        }

        fn on_exit(&self, _transition: &Transition, context: &mut WithTask<C, S>) {
            let handle = {
                let ctx = core::borrow::BorrowMut::borrow_mut(&mut **context);
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
            _context: &mut WithTask<C, S>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                Event::Done => EventReply::Transition(self.idle),
                Event::Begin => EventReply::Ignored,
            }
        }
    }

    pub(crate) fn build_behaviors<F, W, T, S>(
        idle: State,
        loading: State,
        make_task: F,
        wrap_task: W,
    ) -> Behaviors<'static, Event, WithTask<Ctx, S>>
    where
        F: Fn() -> T + Send + Sync + 'static,
        W: Fn(T) -> S + Send + Sync + 'static,
        T: 'static,
        S: 'static,
    {
        let mut behaviors = Behaviors::default();
        behaviors.register(&idle, IdleBehavior { loading });
        behaviors.register(
            &loading,
            LoadingBehavior {
                idle,
                make_task,
                wrap_task,
                marker: PhantomData,
            },
        );
        behaviors
    }

    pub(crate) async fn run_mailbox_case<R, S, F, W, T>(runtime: R, make_task: F, wrap_task: W)
    where
        R: MailboxRuntime<Event, S>,
        F: Fn() -> T + Send + Sync + 'static,
        W: Fn(T) -> S + Send + Sync + 'static,
        T: 'static,
        S: 'static,
    {
        let (tree, idle, loading) = &*INSTANCE_TREE;
        let behaviors = build_behaviors(*idle, *loading, make_task, wrap_task);
        let mut mailbox = Mailbox::new(tree, &behaviors, *idle, Ctx::default(), runtime);

        mailbox.post(Event::Begin);

        for _ in 0..512 {
            let progressed = mailbox.step();
            if !progressed {
                ::tokio::task::yield_now().await;
            }

            ::tokio::task::yield_now().await;

            if mailbox.current() == *idle && mailbox.context().active_task.is_none() {
                break;
            }
        }

        assert_eq!(mailbox.current(), *idle);
        assert_eq!(mailbox.context().active_task, None);
    }
}
