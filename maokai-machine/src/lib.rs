#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::any::type_name;
use core::cell::{Ref, RefCell, RefMut};
use core::marker::PhantomData;
use maokai_gears::ops::EventOp;
use maokai_gears::ops::event::{EventOpConsumer, SharedEventQueue};
#[cfg(any(feature = "tokio-local", feature = "tokio-mt"))]
use maokai_gears::ops::task::TaskHandle;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Reconciler, Ticket};
use maokai_runner::{Behaviors, Runner};
use maokai_tree::{State, StateTree, TreeView};

type Shared<T> = Rc<RefCell<T>>;
type OperationInbox = Shared<VecDeque<Box<dyn Operation>>>;

pub struct MachineHandle<E> {
    inbox: OperationInbox,
    _marker: PhantomData<fn(E)>,
}

impl<E> Clone for MachineHandle<E> {
    fn clone(&self) -> Self {
        Self::new(self.inbox.clone())
    }
}

impl<E> MachineHandle<E> {
    fn new(inbox: OperationInbox) -> Self {
        Self {
            inbox,
            _marker: PhantomData,
        }
    }

    pub fn dispatch<O>(&self, op: O)
    where
        O: Operation + 'static,
    {
        self.inbox.borrow_mut().push_back(Box::new(op));
    }
}

impl<E: 'static> MachineHandle<E> {
    pub fn post(&self, event: E) {
        self.dispatch(EventOp::Emit(event));
    }
}

pub struct Envelope<E, Context> {
    pub context: Shared<Context>,
    pub machine: MachineHandle<E>,
}

impl<E, Context> Clone for Envelope<E, Context> {
    fn clone(&self) -> Self {
        Self {
            context: self.context.clone(),
            machine: self.machine.clone(),
        }
    }
}

impl<E, Context> Envelope<E, Context> {
    fn new(ctx: Shared<Context>, machine: MachineHandle<E>) -> Self {
        Self {
            context: ctx,
            machine,
        }
    }

    pub fn context(&self) -> Ref<'_, Context> {
        self.context.borrow()
    }

    pub fn context_mut(&self) -> RefMut<'_, Context> {
        self.context.borrow_mut()
    }
}

pub struct Machine<'a, 'b, T, E, Context> {
    runner: &'a Runner<'a, T>,
    behaviors: &'b Behaviors<'b, E, Envelope<E, Context>>,
    current: State,
    context: Shared<Context>,
    reconciler: Reconciler,
    inbox: OperationInbox,
    ready_events: SharedEventQueue<E>,
    consumers: BTreeMap<&'static str, Box<dyn OpConsumer>>,
}

impl<'a, 'b, T, E: 'static, Context> Machine<'a, 'b, T, E, Context> {
    pub fn new(
        runner: &'a Runner<'a, T>,
        behaviors: &'b Behaviors<'b, E, Envelope<E, Context>>,
        ctx: Context,
    ) -> Self {
        let ready_events = Rc::new(RefCell::new(VecDeque::new()));
        let inbox = Rc::new(RefCell::new(VecDeque::new()));
        let mut consumers = BTreeMap::new();
        consumers.insert(
            type_name::<EventOp<E>>(),
            Box::new(EventOpConsumer::<E>::new(ready_events.clone())) as Box<dyn OpConsumer>,
        );

        Self {
            runner,
            behaviors,
            current: runner.tree.nil(),
            context: Rc::new(RefCell::new(ctx)),
            reconciler: Reconciler::default(),
            inbox,
            ready_events: ready_events.clone(),
            consumers,
        }
    }

    pub fn current(&self) -> State {
        self.current
    }

    pub fn envelope(&self) -> Envelope<E, Context> {
        Envelope::new(self.context.clone(), MachineHandle::new(self.inbox.clone()))
    }

    pub fn context(&self) -> Ref<'_, Context> {
        self.context.borrow()
    }

    pub fn context_mut(&self) -> RefMut<'_, Context> {
        self.context.borrow_mut()
    }

    pub fn set_consumer<O, Cn>(&mut self, consumer: Cn) -> Option<Box<dyn OpConsumer>>
    where
        O: Operation + 'static,
        Cn: OpConsumer + 'static,
    {
        self.consumers.insert(type_name::<O>(), Box::new(consumer))
    }

    pub fn with_consumer<O, Cn>(mut self, consumer: Cn) -> Self
    where
        O: Operation + 'static,
        Cn: OpConsumer + 'static,
    {
        let _ = self.set_consumer::<O, Cn>(consumer);
        self
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

#[cfg(feature = "tokio-local")]
impl<E: 'static, Context: 'static> Envelope<E, Context> {
    pub fn start_local_task<O: 'static, F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(Self) -> Fut + 'static,
        Fut: Future<Output = O> + 'static,
    {
        let handle = TaskHandle::next();
        let envelop = self.clone();
        let task: maokai_gears::runtime::tokio_local::LocalTask<O> = Box::new(
            move |_: maokai_gears::runtime::tokio_local::LocalTaskEmitter<O>| {
                Box::pin(build(envelop)) as core::pin::Pin<Box<dyn Future<Output = O> + 'static>>
            },
        );
        self.machine
            .dispatch(maokai_gears::ops::task::TaskOp::Start { handle, task });
        handle
    }

    pub fn stop_local_task<O: 'static>(&self, handle: TaskHandle) {
        self.machine.dispatch(maokai_gears::ops::task::TaskOp::<
            maokai_gears::runtime::tokio_local::LocalTask<O>,
        >::Stop(handle));
    }
}

#[cfg(feature = "tokio-mt")]
impl<E, Context: Clone + Send + 'static> Envelope<E, Context> {
    pub fn start_send_task<O: Send + 'static, F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(maokai_gears::runtime::tokio_mt::SendTaskCtx<Context, O>) -> Fut + Send + 'static,
        Fut: Future<Output = O> + Send + 'static,
    {
        let handle = TaskHandle::next();
        let ctx = self.context.borrow().clone();
        let task: maokai_gears::runtime::tokio_mt::SendTask<O> = Box::new(move |emit| {
            Box::pin(build(maokai_gears::runtime::tokio_mt::SendTaskCtx::new(
                ctx, emit,
            ))) as core::pin::Pin<Box<dyn Future<Output = O> + Send + 'static>>
        });
        self.machine
            .dispatch(maokai_gears::ops::task::TaskOp::Start { handle, task });
        handle
    }

    pub fn stop_send_task<O: Send + 'static>(&self, handle: TaskHandle) {
        self.machine.dispatch(maokai_gears::ops::task::TaskOp::<
            maokai_gears::runtime::tokio_mt::SendTask<O>,
        >::Stop(handle));
    }
}

impl<T, E: 'static, Context> Machine<'_, '_, T, E, Context>
where
    StateTree<T>: TreeView,
{
    fn drain_inbox(&mut self) -> bool {
        let mut drained = false;
        while let Some(op) = self.inbox.borrow_mut().pop_front() {
            drained = true;
            let _ = self.reconciler.stage_boxed(op, None);
        }
        drained
    }

    pub fn init(
        &mut self,
        target: State,
        on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>),
    ) -> bool {
        if self.current != self.runner.tree.nil() {
            return false;
        }

        let envo = self.envelope();
        self.current = self
            .runner
            .transition(self.behaviors, &self.current, &target, envo);
        self.advance(on_unhandled);
        true
    }

    /// Stage an event into the reconciler as `EventOp::Emit`.
    /// The event is not dispatched until `advance` is called.
    pub fn post(&mut self, event: E) {
        self.envelope().machine.post(event);
    }

    /// Commit staged operations, route them through the machine's consumers, and
    /// dispatch resulting events until the machine becomes stable.
    pub fn advance(&mut self, on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>)) {
        loop {
            let mut progressed = self.drain_inbox();

            while self.reconciler.has_pending() {
                progressed = true;
                self.reconciler.commit(|ticket, mut op| {
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
                if consumer.drain(&mut self.reconciler) {
                    drained_any = true;
                }
            }

            if drained_any {
                continue;
            }

            if self.drain_inbox() {
                continue;
            }

            while let Some(event) = self.ready_events.borrow_mut().pop_front() {
                progressed = true;
                self.current =
                    self.runner
                        .dispatch(self.behaviors, &self.current, &event, self.envelope());
            }

            if self.drain_inbox() {
                continue;
            }

            if !progressed
                && !self.reconciler.has_pending()
                && self.ready_events.borrow().is_empty()
                && self.inbox.borrow().is_empty()
            {
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

    struct Context {
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

    impl Behavior<LightEvent, Envelope<LightEvent, Context>> for ClosedBehavior {
        fn on_enter(&self, _t: &Transition, envo: Envelope<LightEvent, Context>) {
            envo.context.borrow_mut().logs.push("enter:closed");
        }
        fn on_exit(&self, _t: &Transition, envo: Envelope<LightEvent, Context>) {
            envo.context.borrow_mut().logs.push("exit:closed");
        }
        fn on_event(
            &self,
            event: &LightEvent,
            _current: &State,
            _ctx: Envelope<LightEvent, Context>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, _, opened, _) = &*SETUP_TREE;
            match event {
                LightEvent::Open => EventReply::Transition(*opened),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<LightEvent, Envelope<LightEvent, Context>> for OpenedBehavior {
        fn on_enter(&self, _t: &Transition, envo: Envelope<LightEvent, Context>) {
            envo.context.borrow_mut().logs.push("enter:opened");
        }
        fn on_exit(&self, _t: &Transition, envo: Envelope<LightEvent, Context>) {
            envo.context.borrow_mut().logs.push("exit:opened");
        }
        fn on_event(
            &self,
            event: &LightEvent,
            _current: &State,
            _ctx: Envelope<LightEvent, Context>,
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

    impl Behavior<LightEvent, Envelope<LightEvent, Context>> for ShiningBehavior {
        fn on_enter(&self, _t: &Transition, envo: Envelope<LightEvent, Context>) {
            envo.context.borrow_mut().logs.push("enter:shining");
        }
        fn on_exit(&self, _t: &Transition, envo: Envelope<LightEvent, Context>) {
            envo.context.borrow_mut().logs.push("exit:shining");
        }
        fn on_event(
            &self,
            event: &LightEvent,
            _current: &State,
            _ctx: Envelope<LightEvent, Context>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            match event {
                // Close bubbles up to OpenedBehavior
                LightEvent::Close => EventReply::Ignored,
                _ => EventReply::Handled,
            }
        }
    }

    static BEHAVIORS: LazyLock<Behaviors<'static, LightEvent, Envelope<LightEvent, Context>>> =
        LazyLock::new(|| {
            let (_, closed, opened, shining) = &*SETUP_TREE;
            let mut behaviors = Behaviors::default();
            behaviors.register(closed, ClosedBehavior);
            behaviors.register(opened, OpenedBehavior);
            behaviors.register(shining, ShiningBehavior);
            behaviors
        });

    fn new_machine() -> Machine<'static, 'static, &'static str, LightEvent, Context> {
        Machine::new(&RUNNER, &BEHAVIORS, Context { logs: Vec::new() })
    }

    /// No-op commit handler for tests that don't produce non-event operations.
    fn noop(_: Ticket, _: Box<dyn Operation>) {}

    // --- Basic Machine tests ---

    #[test]
    fn init_transitions_from_nil() {
        let mut machine = new_machine();

        let (_, closed, _, _) = &*SETUP_TREE;
        machine.init(*closed, &mut noop);
        assert_eq!(machine.current(), *closed);
        assert_eq!(machine.context().logs, alloc::vec!["enter:closed"]);

        machine.post(LightEvent::Open);
        machine.advance(&mut noop);

        let (_, _, opened, _) = &*SETUP_TREE;
        assert_eq!(machine.current(), *opened);
        assert_eq!(
            machine.context().logs,
            alloc::vec!["enter:closed", "exit:closed", "enter:opened"]
        );
    }

    #[test]
    fn event_bubbles_from_child_to_parent() {
        let mut machine = new_machine();
        let (_, _, _, shining) = &*SETUP_TREE;
        machine.init(*shining, &mut noop);

        // Close is Ignored by ShiningBehavior → bubbles to OpenedBehavior → Transition(closed)
        machine.post(LightEvent::Close);
        machine.advance(&mut noop);

        let (_, closed, _, _) = &*SETUP_TREE;
        assert_eq!(machine.current(), *closed);
        assert_eq!(
            machine.context().logs,
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
        let (_, _, opened, shining) = &*SETUP_TREE;
        machine.init(*opened, &mut noop);

        machine.post(LightEvent::Shine);
        machine.advance(&mut noop);

        assert_eq!(machine.current(), *shining);
        assert_eq!(
            machine.context().logs,
            alloc::vec!["enter:opened", "enter:shining"]
        );
    }

    #[test]
    fn cloned_envelope_can_post_back_into_machine() {
        let mut machine = new_machine();
        let (_, closed, opened, _) = &*SETUP_TREE;
        machine.init(*closed, &mut noop);

        let envo = machine.envelope();
        envo.machine.post(LightEvent::Open);
        machine.advance(&mut noop);

        assert_eq!(machine.current(), *opened);
        assert_eq!(
            machine.context().logs,
            alloc::vec!["enter:closed", "exit:closed", "enter:opened"]
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
    use maokai_gears::ops::task::{
        TaskCompletion, TaskCompletionConsumer, TaskCompletionOp, TaskOp, TaskOpConsumer,
    };
    use maokai_gears::runtime::tokio_local::{LocalTask, TokioLocalRuntime};
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_tree::StateTree;
    use std::sync::LazyLock;

    #[derive(Debug)]
    enum Ev {
        Go,
        TaskDone,
    }

    #[derive(Clone)]
    struct Context {
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

    impl Behavior<Ev, Envelope<Ev, Context>> for IdleBehavior {
        fn on_enter(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("enter:idle");
        }
        fn on_exit(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("exit:idle");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            _: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::Go => EventReply::Transition(TREE.2),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<Ev, Envelope<Ev, Context>> for WorkingBehavior {
        fn on_enter(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("enter:working");
            let _ = envo.start_local_task(|_envo| async move { "result" });
        }
        fn on_exit(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("exit:working");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            envo: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::TaskDone => {
                    envo.context.borrow_mut().logs.push("task:done");
                    EventReply::Transition(TREE.1)
                }
                _ => EventReply::Ignored,
            }
        }
    }

    static BEHAVIORS: LazyLock<Behaviors<'static, Ev, Envelope<Ev, Context>>> =
        LazyLock::new(|| {
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
                let mut machine = Machine::new(&RUNNER, &BEHAVIORS, Context { logs: Vec::new() })
                    .with_consumer::<TaskOp<LocalTask<&'static str>>, _>(TaskOpConsumer::<
                        TokioLocalRuntime,
                        LocalTask<&'static str>,
                    >::new(
                        TokioLocalRuntime
                    ))
                    .with_consumer::<TaskCompletionOp<&'static str>, _>(
                        TaskCompletionConsumer::new(|completion: TaskCompletion<&'static str>| {
                            assert_eq!(completion.output, "result");
                            OpFlow::Continue(Box::new(EventOp::Emit(Ev::TaskDone)))
                        }),
                    );
                let (_, idle, working) = &*TREE;
                machine.init(*idle, &mut noop);
                assert_eq!(machine.current(), *idle);

                // Go → working, on_enter stages TaskOp
                machine.post(Ev::Go);
                machine.advance(&mut noop);
                assert_eq!(machine.current(), *working);

                // Yield to let spawned local task complete and let the machine drain it.
                tokio::task::yield_now().await;

                for _ in 0..8 {
                    machine.advance(&mut noop);
                    if machine.current() == *idle {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert_eq!(machine.current(), *idle);

                assert_eq!(
                    machine.context().logs,
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
    use maokai_gears::runtime::tokio_mt::{SendTask, TokioMtRuntime};
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_tree::StateTree;
    use std::sync::LazyLock;

    #[derive(Debug)]
    enum Ev {
        Go,
        TaskDone,
    }

    #[derive(Clone)]
    struct Context {
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

    impl Behavior<Ev, Envelope<Ev, Context>> for IdleBehavior {
        fn on_enter(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("enter:idle");
        }
        fn on_exit(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("exit:idle");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            _: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::Go => EventReply::Transition(TREE.2),
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<Ev, Envelope<Ev, Context>> for WorkingBehavior {
        fn on_enter(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("enter:working");
            let _ = envo.start_send_task(|task_ctx| async move {
                task_ctx.emit(EventOp::Emit(Ev::TaskDone));
                "result"
            });
        }
        fn on_exit(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("exit:working");
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            envo: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::TaskDone => {
                    envo.context.borrow_mut().logs.push("task:done");
                    EventReply::Transition(TREE.1)
                }
                _ => EventReply::Ignored,
            }
        }
    }

    static BEHAVIORS: LazyLock<Behaviors<'static, Ev, Envelope<Ev, Context>>> =
        LazyLock::new(|| {
            let (_, idle, working) = &*TREE;
            let mut b = Behaviors::default();
            b.register(idle, IdleBehavior);
            b.register(working, WorkingBehavior);
            b
        });

    fn noop(_: Ticket, _: Box<dyn Operation>) {}

    #[tokio::test]
    async fn task_spawns_on_enter_and_completion_feeds_back() {
        let mut machine = Machine::new(&RUNNER, &BEHAVIORS, Context { logs: Vec::new() })
            .with_consumer::<TaskOp<SendTask<&'static str>>, _>(TaskOpConsumer::<
                TokioMtRuntime,
                SendTask<&'static str>,
            >::new(TokioMtRuntime))
            .with_consumer::<TaskCompletionOp<&'static str>, _>(TaskCompletionConsumer::new(
                |completion: TaskCompletion<&'static str>| {
                    assert_eq!(completion.output, "result");
                    OpFlow::Continue(Box::new(EventOp::Emit(Ev::TaskDone)))
                },
            ));
        let (_, idle, working) = &*TREE;
        machine.init(*idle, &mut noop);
        assert_eq!(machine.current(), *idle);

        // Go → working, on_enter stages TaskOp
        machine.post(Ev::Go);
        machine.advance(&mut noop);
        assert_eq!(machine.current(), *working);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        for _ in 0..8 {
            machine.advance(&mut noop);
            if machine.current() == *idle {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(machine.current(), *idle);

        assert_eq!(
            machine.context().logs,
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
