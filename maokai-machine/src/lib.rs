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
use maokai_reconciler::{HasReconciler, OpConsumer, OpFlow, Operation, Reconciler, Ticket};
use maokai_runner::{Behaviors, Runner};
use maokai_tree::{State, StateTree, TreeView};

pub struct Envelope<C> {
    pub context: C,
    pub reconciler: Reconciler,
}

impl<C> HasReconciler for Envelope<C> {
    fn reconciler(&mut self) -> &mut Reconciler {
        &mut self.reconciler
    }
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
        target: State,
        context: &mut Envelope<C>,
        on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>),
    ) -> bool {
        if self.current != self.runner.tree.nil() {
            return false;
        }

        self.current = self
            .runner
            .transition(self.behaviors, &self.current, &target, context);
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
            while context.reconciler.has_pending() {
                context.reconciler.commit(|ticket, mut op| {
                    loop {
                        let key = op.operation_key();

                        if let Some(consumer) = self.consumers.get_mut(key) {
                            match consumer.as_mut().consume(ticket, op) {
                                OpFlow::Consumed => return,
                                OpFlow::Continue(next) => op = next,
                            }
                        } else {
                            on_unhandled(ticket, op);
                            return;
                        }
                    }
                });
            }

            let mut drained_any = false;
            for consumer in self.consumers.values_mut() {
                if consumer.drain(&mut context.reconciler) {
                    drained_any = true;
                }
            }

            if drained_any {
                continue;
            }

            while let Some(event) = self.ready_events.borrow_mut().pop_front() {
                self.current = self
                    .runner
                    .dispatch(self.behaviors, &self.current, &event, context);
            }

            if !context.reconciler.has_pending() && self.ready_events.borrow().is_empty() {
                break;
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

    // --- Basic Machine tests ---

    #[test]
    fn init_transitions_from_nil() {
        let mut machine = new_machine();
        let mut envelope = new_envelope();

        let (_, closed, _, _) = &*SETUP_TREE;
        machine.init(*closed, &mut envelope, &mut noop);
        assert_eq!(machine.current(), *closed);
        assert_eq!(envelope.context.logs, alloc::vec!["enter:closed"]);

        machine.post(LightEvent::Open, &mut envelope);
        machine.advance(&mut envelope, &mut noop);

        let (_, _, opened, _) = &*SETUP_TREE;
        assert_eq!(machine.current(), *opened);
        assert_eq!(
            envelope.context.logs,
            alloc::vec!["enter:closed", "exit:closed", "enter:opened"]
        );
    }

    #[test]
    fn event_bubbles_from_child_to_parent() {
        let mut machine = new_machine();
        let mut envelope = new_envelope();

        let (_, _, _, shining) = &*SETUP_TREE;
        machine.init(*shining, &mut envelope, &mut noop);

        // Close is Ignored by ShiningBehavior → bubbles to OpenedBehavior → Transition(closed)
        machine.post(LightEvent::Close, &mut envelope);
        machine.advance(&mut envelope, &mut noop);

        let (_, closed, _, _) = &*SETUP_TREE;
        assert_eq!(machine.current(), *closed);
        assert_eq!(
            envelope.context.logs,
            alloc::vec![
                "enter:opened",
                "enter:shining",
                "exit:shining",
                "exit:opened",
                "enter:closed"
            ]
        );
    }

    #[test]
    fn shine_transitions_into_child_state() {
        let mut machine = new_machine();
        let mut envelope = new_envelope();

        let (_, _, opened, shining) = &*SETUP_TREE;
        machine.init(*opened, &mut envelope, &mut noop);

        machine.post(LightEvent::Shine, &mut envelope);
        machine.advance(&mut envelope, &mut noop);

        assert_eq!(machine.current(), *shining);
        assert_eq!(
            envelope.context.logs,
            alloc::vec!["enter:opened", "enter:shining"]
        );
    }
}

// --- Tokio LocalSet integration tests ---

#[cfg(all(test, feature = "tokio-local"))]
mod tokio_local_tests {
    #![allow(clippy::unwrap_used)]
    extern crate std;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use maokai_gears::ops::EventOp;
    use maokai_gears::ops::task::{
        TaskCompletion, TaskCompletionConsumer, TaskCompletionOp, TaskOp, TaskOpConsumer,
    };
    use maokai_gears::runtime::tokio_local::{LocalTask, LocalTaskOpsExt, TokioLocalRuntime};
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_tree::StateTree;
    use std::sync::LazyLock;

    #[derive(Debug)]
    enum Ev {
        Go,
        TaskDone,
    }

    struct Ctx {
        logs: Vec<&'static str>,
    }

    static TREE: LazyLock<(StateTree<&str>, State, State)> = LazyLock::new(|| {
        let mut tree = StateTree::new("root");
        let idle = tree.add_child(&tree.root(), "idle");
        let working = tree.add_child(&tree.root(), "working");
        (tree, idle, working)
    });

    static RUNNER: LazyLock<Runner<&'static str>> = LazyLock::new(|| Runner::new(&TREE.0));

    struct IdleBehavior;
    struct WorkingBehavior;

    impl Behavior<Ev, Envelope<Ctx>> for IdleBehavior {
        fn on_enter(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("enter:idle");
        }
        fn on_exit(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("exit:idle");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            _: &mut Envelope<Ctx>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::Go => EventReply::Transition(TREE.2),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<Ev, Envelope<Ctx>> for WorkingBehavior {
        fn on_enter(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("enter:working");
            let _ = ctx.start_local_task(async { "result" });
        }
        fn on_exit(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("exit:working");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            ctx: &mut Envelope<Ctx>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::TaskDone => {
                    ctx.context.logs.push("task:done");
                    EventReply::Transition(TREE.1)
                }
                _ => EventReply::Ignored,
            }
        }
    }

    static BEHAVIORS: LazyLock<Behaviors<'static, Ev, Envelope<Ctx>>> = LazyLock::new(|| {
        let (_, idle, working) = &*TREE;
        let mut b = Behaviors::default();
        b.register(idle, IdleBehavior);
        b.register(working, WorkingBehavior);
        b
    });

    fn noop(_: Ticket, _: Box<dyn Operation>) {}

    #[tokio::test]
    async fn task_spawns_on_enter_and_completion_feeds_back() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut machine = Machine::new(&RUNNER, &BEHAVIORS);
                let mut envelope = Envelope::new(Ctx { logs: Vec::new() });

                machine.set_consumer::<TaskOp<LocalTask<&'static str>>, _>(TaskOpConsumer::<
                    TokioLocalRuntime,
                    LocalTask<&'static str>,
                >::new(
                    TokioLocalRuntime
                ));
                machine.set_consumer::<TaskCompletionOp<&'static str>, _>(
                    TaskCompletionConsumer::new(|completion: TaskCompletion<&'static str>| {
                        assert_eq!(completion.output, "result");
                        OpFlow::Continue(Box::new(EventOp::Emit(Ev::TaskDone)))
                    }),
                );

                let (_, idle, working) = &*TREE;
                machine.init(*idle, &mut envelope, &mut noop);
                assert_eq!(machine.current(), *idle);

                // Go → working, on_enter stages TaskOp
                machine.post(Ev::Go, &mut envelope);
                machine.advance(&mut envelope, &mut noop);
                assert_eq!(machine.current(), *working);

                // Yield to let spawned local task complete and let the machine drain it.
                tokio::task::yield_now().await;

                machine.advance(&mut envelope, &mut noop);
                assert_eq!(machine.current(), *idle);

                assert_eq!(
                    envelope.context.logs,
                    vec![
                        "enter:idle",
                        "exit:idle",
                        "enter:working",
                        "task:done",
                        "exit:working",
                        "enter:idle"
                    ]
                );
            })
            .await;
    }
}

// --- Tokio multi-thread integration tests ---

#[cfg(all(test, feature = "tokio-mt"))]
mod tokio_mt_tests {
    #![allow(clippy::unwrap_used)]
    extern crate std;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use maokai_gears::ops::EventOp;
    use maokai_gears::ops::task::{
        TaskCompletion, TaskCompletionConsumer, TaskCompletionOp, TaskOp, TaskOpConsumer,
    };
    use maokai_gears::runtime::tokio_mt::{SendTask, SendTaskOpsExt, TokioMtRuntime};
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_tree::StateTree;
    use std::sync::LazyLock;

    #[derive(Debug)]
    enum Ev {
        Go,
        TaskDone,
    }

    struct Ctx {
        logs: Vec<&'static str>,
    }

    static TREE: LazyLock<(StateTree<&str>, State, State)> = LazyLock::new(|| {
        let mut tree = StateTree::new("root");
        let idle = tree.add_child(&tree.root(), "idle");
        let working = tree.add_child(&tree.root(), "working");
        (tree, idle, working)
    });

    static RUNNER: LazyLock<Runner<&'static str>> = LazyLock::new(|| Runner::new(&TREE.0));

    struct IdleBehavior;
    struct WorkingBehavior;

    impl Behavior<Ev, Envelope<Ctx>> for IdleBehavior {
        fn on_enter(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("enter:idle");
        }
        fn on_exit(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("exit:idle");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            _: &mut Envelope<Ctx>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::Go => EventReply::Transition(TREE.2),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<Ev, Envelope<Ctx>> for WorkingBehavior {
        fn on_enter(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("enter:working");
            let _ = ctx.start_send_task(async { "result" });
        }
        fn on_exit(&self, _: &Transition, ctx: &mut Envelope<Ctx>) {
            ctx.context.logs.push("exit:working");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            ctx: &mut Envelope<Ctx>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::TaskDone => {
                    ctx.context.logs.push("task:done");
                    EventReply::Transition(TREE.1)
                }
                _ => EventReply::Ignored,
            }
        }
    }

    static BEHAVIORS: LazyLock<Behaviors<'static, Ev, Envelope<Ctx>>> = LazyLock::new(|| {
        let (_, idle, working) = &*TREE;
        let mut b = Behaviors::default();
        b.register(idle, IdleBehavior);
        b.register(working, WorkingBehavior);
        b
    });

    fn noop(_: Ticket, _: Box<dyn Operation>) {}

    #[tokio::test]
    async fn task_spawns_on_enter_and_completion_feeds_back() {
        let mut machine = Machine::new(&RUNNER, &BEHAVIORS);
        let mut context = Envelope::new(Ctx { logs: Vec::new() });

        machine.set_consumer::<TaskOp<SendTask<&'static str>>, _>(TaskOpConsumer::<
            TokioMtRuntime,
            SendTask<&'static str>,
        >::new(TokioMtRuntime));
        machine.set_consumer::<TaskCompletionOp<&'static str>, _>(TaskCompletionConsumer::new(
            |completion: TaskCompletion<&'static str>| {
                assert_eq!(completion.output, "result");
                OpFlow::Continue(Box::new(EventOp::Emit(Ev::TaskDone)))
            },
        ));

        let (_, idle, working) = &*TREE;
        machine.init(*idle, &mut context, &mut noop);
        assert_eq!(machine.current(), *idle);

        // Go → working, on_enter stages TaskOp
        machine.post(Ev::Go, &mut context);
        machine.advance(&mut context, &mut noop);
        assert_eq!(machine.current(), *working);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        machine.advance(&mut context, &mut noop);
        assert_eq!(machine.current(), *idle);

        assert_eq!(
            context.context.logs,
            vec![
                "enter:idle",
                "exit:idle",
                "enter:working",
                "task:done",
                "exit:working",
                "enter:idle"
            ]
        );
    }
}
