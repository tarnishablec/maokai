#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::any::type_name;
use core::cell::RefCell;
use maokai_gears::ops::EventOp;
use maokai_gears::ops::event::{EventOpConsumer, SharedEventQueue};
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Reconciler, Ticket};
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
    ready_events: SharedEventQueue<E>,
    consumers: BTreeMap<&'static str, Box<dyn OpConsumer>>,
}

impl<'a, 'b, T, E: 'static, C> Machine<'a, 'b, T, E, C> {
    pub fn new(runner: &'a Runner<'a, T>, behaviors: &'b Behaviors<'b, E, Envelope<C>>) -> Self {
        let ready_events = Rc::new(RefCell::new(VecDeque::new()));
        let mut consumers = BTreeMap::new();
        consumers.insert(
            type_name::<EventOp<E>>(),
            Box::new(EventOpConsumer::<E>::new(ready_events.clone())) as Box<dyn OpConsumer>,
        );

        Self {
            runner,
            behaviors,
            current: runner.tree.nil(),
            ready_events: ready_events.clone(),
            consumers,
        }
    }

    pub fn current(&self) -> State {
        self.current
    }

    pub fn set_consumer<O, Cn>(&mut self, consumer: Cn) -> Option<Box<dyn OpConsumer>>
    where
        O: Operation + 'static,
        Cn: OpConsumer + 'static,
    {
        self.consumers.insert(type_name::<O>(), Box::new(consumer))
    }

    pub fn remove_consumer<O>(&mut self) -> Option<Box<dyn OpConsumer>>
    where
        O: Operation + 'static,
    {
        self.consumers.remove(type_name::<O>())
    }

    pub fn clear_consumers(&mut self) {
        self.consumers = BTreeMap::new();
        self.consumers.insert(
            type_name::<EventOp<E>>(),
            Box::new(EventOpConsumer::<E>::new(self.ready_events.clone())),
        );
    }
}

impl<C> Envelope<C> {
    pub fn new(context: C) -> Self {
        Self {
            context,
            reconciler: Reconciler::default(),
        }
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

    /// Commit staged operations, route them through the machine's consumers, and
    /// dispatch resulting events until the machine becomes stable.
    pub fn advance(
        &mut self,
        context: &mut Envelope<C>,
        on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>),
    ) {
        loop {
            context.reconciler.commit(|ticket, op| {
                let key = op.operation_key();

                if let Some(consumer) = self.consumers.get_mut(key) {
                    match consumer.as_mut().consume(ticket, op) {
                        OpFlow::Consumed => {}
                        OpFlow::Continue(op) => on_unhandled(ticket, op),
                    }
                } else {
                    on_unhandled(ticket, op);
                }
            });

            if self.ready_events.borrow().is_empty() {
                break;
            }

            while let Some(event) = self.ready_events.borrow_mut().pop_front() {
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
        Envelope::new(LightContext { logs: Vec::new() })
    }

    fn new_machine() -> Machine<'static, 'static, &'static str, LightEvent, LightContext> {
        Machine::new(&RUNNER, &BEHAVIORS)
    }

    /// No-op commit handler for tests that don't produce non-event operations.
    fn noop(_: Ticket, _: Box<dyn Operation>) {}
}
