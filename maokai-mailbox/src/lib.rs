#![no_std]
extern crate alloc;

use maokai_reconciler::Reconciler;
use maokai_runner::{Behaviors, Runner};
use maokai_tree::State;

pub struct Envelope<C> {
    pub context: C,
    pub reconciler: Reconciler,
}

pub struct Mailbox<'a, 'b, T, E, C> {
    runner: &'a Runner<'a, T>,
    current: State,
    behaviors: &'b Behaviors<'b, E, Envelope<C>>,
}

impl<'a, 'b, T, E, C> Mailbox<'a, 'b, T, E, C> {
    pub fn new(runner: &'a Runner<'a, T>, behaviors: &'b Behaviors<'b, E, Envelope<C>>) -> Self {
        Self {
            runner,
            current: runner.tree.nil(),
            behaviors,
        }
    }

    pub fn current(&self) -> State {
        self.current
    }

    pub fn step() {}
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    extern crate std;

    use super::*;
    use alloc::vec::Vec;
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_tree::{StateTree, TreeView};
    use std::sync::LazyLock;

    #[derive(Debug)]
    enum LightEvent {
        Open,
        Close,
        Shine,
    }

    struct LightContext {
        logs: Vec<&'static str>,
    }

    // --- Tree: root -> closed, opened -> shining ---

    static SETUP_TREE: LazyLock<(StateTree<&str>, State, State, State)> = LazyLock::new(|| {
        let mut tree = StateTree::new("root");
        let closed = tree.add_child(&tree.root(), "closed");
        let opened = tree.add_child(&tree.root(), "opened");
        let shining = tree.add_child(&opened, "shining");
        (tree, closed, opened, shining)
    });

    static RUNNER: LazyLock<Runner<&'static str>> = LazyLock::new(|| Runner::new(&SETUP_TREE.0));

    // --- Behaviors ---

    struct ClosedBehavior;
    struct OpenedBehavior;
    struct ShiningBehavior;

    impl Behavior<LightEvent, Envelope<LightContext>> for ClosedBehavior {
        fn on_enter(&self, _t: &Transition, ctx: &mut Envelope<LightContext>) {
            ctx.context.logs.push("enter:closed");
        }
        fn on_exit(&self, _t: &Transition, ctx: &mut Envelope<LightContext>) {
            ctx.context.logs.push("exit:closed");
        }
        fn on_event(
            &self,
            event: &LightEvent,
            _current: &State,
            _ctx: &mut Envelope<LightContext>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, _, opened, _) = &*SETUP_TREE;
            match event {
                LightEvent::Open => EventReply::Transition(*opened),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<LightEvent, Envelope<LightContext>> for OpenedBehavior {
        fn on_enter(&self, _t: &Transition, ctx: &mut Envelope<LightContext>) {
            ctx.context.logs.push("enter:opened");
        }
        fn on_exit(&self, _t: &Transition, ctx: &mut Envelope<LightContext>) {
            ctx.context.logs.push("exit:opened");
        }
        fn on_event(
            &self,
            event: &LightEvent,
            _current: &State,
            _ctx: &mut Envelope<LightContext>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, closed, _, shining) = &*SETUP_TREE;

            match event {
                LightEvent::Close => EventReply::Transition(*closed),
                LightEvent::Shine => EventReply::Transition(*shining),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<LightEvent, Envelope<LightContext>> for ShiningBehavior {
        fn on_enter(&self, _t: &Transition, ctx: &mut Envelope<LightContext>) {
            ctx.context.logs.push("enter:shining");
        }
        fn on_exit(&self, _t: &Transition, ctx: &mut Envelope<LightContext>) {
            ctx.context.logs.push("exit:shining");
        }
        fn on_event(
            &self,
            event: &LightEvent,
            _current: &State,
            _ctx: &mut Envelope<LightContext>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                // Close bubbles up to OpenedBehavior
                LightEvent::Close => EventReply::Ignored,
                _ => EventReply::Handled,
            }
        }
    }

    static BEHAVIORS: LazyLock<Behaviors<'static, LightEvent, Envelope<LightContext>>> =
        LazyLock::new(|| {
            let (_, closed, opened, shining) = &*SETUP_TREE;
            let mut b = Behaviors::default();
            b.register(closed, ClosedBehavior);
            b.register(opened, OpenedBehavior);
            b.register(shining, ShiningBehavior);
            b
        });

    #[test]
    fn initial_enter_from_nil_to_closed() {
        let mut envelope = Envelope {
            context: LightContext {
                logs: Vec::new(),
            },
            reconciler: Reconciler::default(),
        };

        let (tree, closed, _, _) = &*SETUP_TREE;

        // nil -> closed: should enter [root, closed], exit [nil] (no behavior)
        let current = RUNNER.transition(&BEHAVIORS, &tree.nil(), closed, &mut envelope);

        assert_eq!(current, *closed);
        assert_eq!(envelope.context.logs, std::vec!["enter:closed"]);
    }

    #[test]
    fn open_then_close() {
        let mut envelope = Envelope {
            context: LightContext {
                logs: Vec::new(),
            },
            reconciler: Reconciler::default(),
        };

        let (_, closed, opened, _) = &*SETUP_TREE;

        // Start at closed, dispatch Open
        let current = RUNNER.dispatch(&BEHAVIORS, closed, &LightEvent::Open, &mut envelope);
        assert_eq!(current, *opened);
        assert_eq!(
            envelope.context.logs,
            std::vec!["exit:closed", "enter:opened"]
        );

        envelope.context.logs.clear();

        // At opened, dispatch Close
        let current = RUNNER.dispatch(&BEHAVIORS, &current, &LightEvent::Close, &mut envelope);
        assert_eq!(current, *closed);
        assert_eq!(
            envelope.context.logs,
            std::vec!["exit:opened", "enter:closed"]
        );
    }

    #[test]
    fn shine_is_child_of_opened() {
        let mut envelope = Envelope {
            context: LightContext {
                logs: Vec::new(),
            },
            reconciler: Reconciler::default(),
        };

        let (_, _, opened, shining) = &*SETUP_TREE;

        // At opened, dispatch Shine -> enters shining (child of opened)
        let current = RUNNER.dispatch(&BEHAVIORS, opened, &LightEvent::Shine, &mut envelope);
        assert_eq!(current, *shining);
        // No exit:opened because shining is a child — only enter:shining
        assert_eq!(envelope.context.logs, std::vec!["enter:shining"]);
    }

    #[test]
    fn close_bubbles_from_shining_to_opened() {
        let mut envelope = Envelope {
            context: LightContext {
                logs: Vec::new(),
            },
            reconciler: Reconciler::default(),
        };

        let (_, closed, _, shining) = &*SETUP_TREE;

        // At shining, dispatch Close -> ShiningBehavior returns Ignored -> bubbles to OpenedBehavior -> transitions to closed
        let current = RUNNER.dispatch(&BEHAVIORS, shining, &LightEvent::Close, &mut envelope);
        assert_eq!(current, *closed);
        // exit shining, exit opened, enter closed
        assert_eq!(
            envelope.context.logs,
            std::vec!["exit:shining", "exit:opened", "enter:closed"]
        );
    }
}
