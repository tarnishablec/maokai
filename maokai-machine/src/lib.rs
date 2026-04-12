#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use downcast::Downcast;
use maokai_contracts::ops::EventOp;
use maokai_reconciler::{Operation, Reconciler, Ticket};
use maokai_runner::{Behaviors, Runner};
use maokai_tree::{State, StateTree, TreeView};

pub struct Envelope<C> {
    pub context: C,
    pub reconciler: Reconciler,
}

pub struct Machine<'a, 'b, T, E, C> {
    runner: &'a Runner<'a, T>,
    behaviors: &'b Behaviors<'b, E, Envelope<C>>,
    current: State,
}

impl<'a, 'b, T, E, C> Machine<'a, 'b, T, E, C> {
    pub fn new(runner: &'a Runner<'a, T>, behaviors: &'b Behaviors<'b, E, Envelope<C>>) -> Self {
        Self {
            runner,
            behaviors,
            current: runner.tree.nil(),
        }
    }

    pub fn current(&self) -> State {
        self.current
    }
}

impl<T, E: 'static, C> Machine<'_, '_, T, E, C>
where
    StateTree<T>: TreeView,
{
    pub fn init(
        &mut self,
        event: E,
        context: &mut Envelope<C>,
        on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>),
    ) -> bool {
        if self.current != self.runner.tree.nil() {
            return false;
        }

        self.post(event, context);
        self.advance(context, on_unhandled);
        true
    }

    /// Stage an event into the reconciler as `EventOp::Emit`.
    /// The event is not dispatched until `advance` is called.
    pub fn post(&mut self, event: E, context: &mut Envelope<C>) {
        context.reconciler.stage(EventOp::Emit(event), None);
    }

    /// Commit reconciler and process all staged operations.
    ///
    /// - `EventOp::Emit`: dispatched through the runner (may stage more operations).
    /// - Everything else: passed through `consumers` in order.
    /// - Unconsumed operations fall through to `on_unhandled`.
    ///
    /// Repeats until no more `EventOp` is produced.
    pub fn advance(
        &mut self,
        context: &mut Envelope<C>,
        on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>),
    ) {
        loop {
            // Collect EventOps from this commit round
            let mut events = VecDeque::new();
            context
                .reconciler
                .flush(|ticket, op| match Downcast::<EventOp<E>>::downcast(op) {
                    Ok(event_op) => match *event_op {
                        EventOp::Emit(e) => events.push_back(e),
                    },
                    Err(err) => on_unhandled(ticket, err.into_object()),
                });

            if events.is_empty() {
                break;
            }

            // Dispatch collected events (may stage new operations → next loop iteration)
            while let Some(event) = events.pop_front() {
                self.current = self
                    .runner
                    .dispatch(self.behaviors, &self.current, &event, context);
            }
        }
    }
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
            let mut behaviors = Behaviors::default();
            behaviors.register(closed, ClosedBehavior);
            behaviors.register(opened, OpenedBehavior);
            behaviors.register(shining, ShiningBehavior);
            behaviors
        });

    fn new_envelope() -> Envelope<LightContext> {
        Envelope {
            context: LightContext { logs: Vec::new() },
            reconciler: Reconciler::default(),
        }
    }

    fn new_machine() -> Machine<'static, 'static, &'static str, LightEvent, LightContext> {
        Machine::new(&RUNNER, &BEHAVIORS)
    }

    /// No-op commit handler for tests that don't produce non-event operations.
    fn noop(_: Ticket, _: Box<dyn Operation>) {}
}
