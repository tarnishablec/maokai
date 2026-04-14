#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::sync::Arc;
use core::any::type_name;
use core::cell::{Ref, RefCell, RefMut};
use core::marker::PhantomData;
use maokai_gears::ops::EventOp;
use maokai_gears::ops::event::{EventOpConsumer, SharedEventQueue};
use maokai_gears::ops::task::{TaskHandle, TaskOp};
#[cfg(feature = "tokio-local")]
use maokai_gears::runtime::tokio_local::{LocalTask, TokioLocalTaskConsumer};
#[cfg(feature = "tokio-mt")]
use maokai_gears::runtime::tokio_mt::{SendTask, SendTaskMailboxSender, TokioMtTaskConsumer};
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Reconciler, Ticket};
use maokai_runner::{Behaviors, Runner};
use maokai_tree::{State, StateTree, TreeView};

type Shared<T> = Rc<RefCell<T>>;

pub struct MachineHandle<E> {
    reconciler: Shared<Reconciler>,
    _marker: PhantomData<fn(E)>,
}

impl<E> Clone for MachineHandle<E> {
    fn clone(&self) -> Self {
        Self::new(self.reconciler.clone())
    }
}

impl<E> MachineHandle<E> {
    fn new(reconciler: Shared<Reconciler>) -> Self {
        Self {
            reconciler,
            _marker: PhantomData,
        }
    }

    pub fn stage<O>(&self, op: O)
    where
        O: Operation + 'static,
    {
        let _ = self.reconciler.borrow_mut().stage_boxed(Box::new(op), None);
    }
}

impl<E: 'static> MachineHandle<E> {
    pub fn post(&self, event: E) {
        self.stage(EventOp::Emit(event));
    }
}

pub struct Envelope<E, Context> {
    context: Shared<Context>,
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
    fn new(context: Shared<Context>, machine: MachineHandle<E>) -> Self {
        Self { context, machine }
    }

    pub fn context(&self) -> Ref<'_, Context> {
        self.context.borrow()
    }

    pub fn context_mut(&self) -> RefMut<'_, Context> {
        self.context.borrow_mut()
    }

    pub fn stop_task<T: 'static>(&self, handle: TaskHandle) {
        self.machine.stage(TaskOp::<T>::Stop(handle));
    }
}

pub trait SendOpSink: Send + Sync + 'static {
    fn send_op(&self, op: Box<dyn Operation + Send>);
}

pub struct SendMachineHandle<E> {
    sink: Arc<dyn SendOpSink>,
    _marker: PhantomData<fn(E)>,
}

impl<E> Clone for SendMachineHandle<E> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            _marker: PhantomData,
        }
    }
}

impl<E> SendMachineHandle<E> {
    pub fn stage<O>(&self, op: O)
    where
        O: Operation + Send + 'static,
    {
        self.sink.send_op(Box::new(op));
    }
}

impl<E: Send + 'static> SendMachineHandle<E> {
    pub fn post(&self, event: E) {
        self.stage(EventOp::Emit(event));
    }
}

pub struct SendEnvelope<E, Context> {
    context: RefCell<Context>,
    pub machine: SendMachineHandle<E>,
}

impl<E, Context: Clone> Clone for SendEnvelope<E, Context> {
    fn clone(&self) -> Self {
        Self {
            context: RefCell::new(self.context.borrow().clone()),
            machine: self.machine.clone(),
        }
    }
}

impl<E, Context> SendEnvelope<E, Context> {
    pub fn context(&self) -> Ref<'_, Context> {
        self.context.borrow()
    }

    pub fn context_mut(&self) -> RefMut<'_, Context> {
        self.context.borrow_mut()
    }
}

pub struct RequestTransitionOp {
    pub target: State,
}

impl Operation for RequestTransitionOp {}

type SharedTransitionQueue = Shared<VecDeque<State>>;

pub struct RequestTransitionConsumer {
    queue: SharedTransitionQueue,
}

impl RequestTransitionConsumer {
    pub fn new(queue: SharedTransitionQueue) -> Self {
        Self { queue }
    }
}

impl OpConsumer for RequestTransitionConsumer {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match downcast::Downcast::<RequestTransitionOp>::downcast(op) {
            Ok(req) => {
                self.queue.borrow_mut().push_back(req.target);
                OpFlow::Consumed
            }
            Err(err) => OpFlow::Continue(err.into_object()),
        }
    }
}

pub struct Machine<'a, 'b, T, E, Context> {
    runner: &'a Runner<'a, T>,
    behaviors: &'b Behaviors<'b, E, Envelope<E, Context>>,
    current: State,
    context: Shared<Context>,
    reconciler: Shared<Reconciler>,
    ready_events: SharedEventQueue<E>,
    pending_transitions: SharedTransitionQueue,
    consumers: BTreeMap<&'static str, Box<dyn OpConsumer>>,
}

impl<'a, 'b, T, E: 'static, Context> Machine<'a, 'b, T, E, Context> {
    fn install_default_consumers(&mut self) {
        let _ = self
            .set_consumer::<EventOp<E>, _>(EventOpConsumer::<E>::new(self.ready_events.clone()));
        let _ = self.set_consumer::<RequestTransitionOp, _>(RequestTransitionConsumer::new(
            self.pending_transitions.clone(),
        ));
        #[cfg(feature = "tokio-local")]
        let _ = self.set_consumer::<TaskOp<LocalTask>, _>(TokioLocalTaskConsumer::default());
        #[cfg(feature = "tokio-mt")]
        let _ = self.set_consumer::<TaskOp<SendTask>, _>(TokioMtTaskConsumer::default());
    }

    pub fn new(
        runner: &'a Runner<'a, T>,
        behaviors: &'b Behaviors<'b, E, Envelope<E, Context>>,
        ctx: Context,
    ) -> Self {
        let ready_events = Rc::new(RefCell::new(VecDeque::new()));
        let pending_transitions = Rc::new(RefCell::new(VecDeque::new()));
        let mut machine = Self {
            runner,
            behaviors,
            current: runner.tree.nil(),
            context: Rc::new(RefCell::new(ctx)),
            reconciler: Rc::new(RefCell::new(Reconciler::default())),
            ready_events,
            pending_transitions,
            consumers: BTreeMap::new(),
        };
        machine.install_default_consumers();
        machine
    }

    pub fn current(&self) -> State {
        self.current
    }

    pub fn envelope(&self) -> Envelope<E, Context> {
        Envelope::new(
            self.context.clone(),
            MachineHandle::new(self.reconciler.clone()),
        )
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
        self.install_default_consumers();
    }
}

#[cfg(feature = "tokio-local")]
impl<E: 'static, Context: 'static> Envelope<E, Context> {
    pub fn start_local_task<F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(Self) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let handle = TaskHandle::next();
        let envelop = self.clone();
        let task: LocalTask = Box::new(move |_| {
            Box::pin(build(envelop)) as core::pin::Pin<Box<dyn Future<Output = ()> + 'static>>
        });
        self.machine.stage(TaskOp::Start { handle, task });
        handle
    }

    pub fn stop_local_task(&self, handle: TaskHandle) {
        self.stop_task::<LocalTask>(handle);
    }
}

#[cfg(feature = "tokio-mt")]
struct MpscOpSink(SendTaskMailboxSender);

#[cfg(feature = "tokio-mt")]
impl SendOpSink for MpscOpSink {
    fn send_op(&self, op: Box<dyn Operation + Send>) {
        let _ = self.0.send(op);
    }
}

#[cfg(feature = "tokio-mt")]
impl<E, Context: Clone + Send + 'static> Envelope<E, Context> {
    pub fn start_send_task<F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(SendEnvelope<E, Context>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handle = TaskHandle::next();
        let ctx = self.context.borrow().clone();
        let task: SendTask = Box::new(move |emitter| {
            let sink: Arc<dyn SendOpSink> = Arc::new(MpscOpSink(emitter.into_sender()));
            let envelope = SendEnvelope {
                context: RefCell::new(ctx),
                machine: SendMachineHandle {
                    sink,
                    _marker: PhantomData,
                },
            };
            Box::pin(build(envelope))
                as core::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        });
        self.machine.stage(TaskOp::Start { handle, task });
        handle
    }

    pub fn stop_send_task(&self, handle: TaskHandle) {
        self.stop_task::<SendTask>(handle);
    }
}

impl<T, E: 'static, Context> Machine<'_, '_, T, E, Context>
where
    StateTree<T>: TreeView,
{
    pub fn init(&mut self, target: State) -> bool {
        self.init_with(target, &mut |_, _| {})
    }

    pub fn init_with(
        &mut self,
        target: State,
        on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>),
    ) -> bool {
        if self.current != self.runner.tree.nil() {
            return false;
        }

        self.envelope()
            .machine
            .stage(RequestTransitionOp { target });
        self.advance_with(on_unhandled);
        true
    }

    /// Stage an event into the reconciler as `EventOp::Emit`.
    /// The event is not dispatched until `advance` is called.
    pub fn post(&mut self, event: E) {
        self.envelope().machine.post(event);
    }

    /// Commit staged operations, route them through the machine's consumers, and
    /// dispatch resulting events until the machine becomes stable.
    pub fn advance(&mut self) {
        self.advance_with(&mut |_, _| {});
    }

    pub fn advance_with(&mut self, on_unhandled: &mut impl FnMut(Ticket, Box<dyn Operation>)) {
        loop {
            let mut progressed = false;

            while self.reconciler.borrow().has_pending() {
                progressed = true;
                self.reconciler.borrow_mut().commit(|ticket, mut op| {
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
                if consumer.drain(&mut self.reconciler.borrow_mut()) {
                    drained_any = true;
                }
            }

            if drained_any {
                continue;
            }

            while let Some(target) = self.pending_transitions.borrow_mut().pop_front() {
                progressed = true;
                let envo = self.envelope();
                self.current = self
                    .runner
                    .transition(self.behaviors, &self.current, &target, envo);
            }

            while let Some(event) = self.ready_events.borrow_mut().pop_front() {
                progressed = true;
                self.current =
                    self.runner
                        .dispatch(self.behaviors, &self.current, &event, self.envelope());
            }

            if !progressed
                && !self.reconciler.borrow().has_pending()
                && self.ready_events.borrow().is_empty()
                && self.pending_transitions.borrow().is_empty()
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

    // --- Basic Machine tests ---

    #[test]
    fn init_transitions_from_nil() {
        let mut machine = new_machine();

        let (_, closed, _, _) = &*SETUP_TREE;
        machine.init(*closed);
        assert_eq!(machine.current(), *closed);
        assert_eq!(machine.context().logs, alloc::vec!["enter:closed"]);

        machine.post(LightEvent::Open);
        machine.advance();

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
        machine.init(*shining);

        // Close is Ignored by ShiningBehavior → bubbles to OpenedBehavior → Transition(closed)
        machine.post(LightEvent::Close);
        machine.advance();

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
        machine.init(*opened);

        machine.post(LightEvent::Shine);
        machine.advance();

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
        machine.init(*closed);

        let envo = machine.envelope();
        envo.machine.post(LightEvent::Open);
        machine.advance();

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
            let _ = envo.start_local_task(|envo| async move {
                envo.machine.post(Ev::TaskDone);
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

    #[tokio::test]
    async fn task_spawns_on_enter_and_posts_back() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut machine = Machine::new(&RUNNER, &BEHAVIORS, Context { logs: Vec::new() });
                let (_, idle, working) = &*TREE;
                machine.init(*idle);
                assert_eq!(machine.current(), *idle);

                // Go → working, on_enter stages TaskOp
                machine.post(Ev::Go);
                machine.advance();
                assert_eq!(machine.current(), *working);

                // Yield to let spawned local task complete and let the machine drain it.
                tokio::task::yield_now().await;

                for _ in 0..8 {
                    machine.advance();
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
    use maokai_runner::{Behavior, EventReply, Transition};
    use maokai_tree::StateTree;
    use std::sync::LazyLock;

    #[derive(Debug)]
    enum Ev {
        Go,
        Back,
        TaskDone,
    }

    #[derive(Clone)]
    struct Context {
        logs: Vec<&'static str>,
        running_task: Option<TaskHandle>,
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
            envo.context_mut().logs.push("enter:working");
            let handle = envo.start_send_task(|envo| async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                envo.machine.post(Ev::TaskDone);
            });
            envo.context_mut().running_task = Some(handle);
        }
        fn on_exit(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context_mut().logs.push("exit:working");
            if let Some(handle) = envo.context_mut().running_task.take() {
                envo.stop_task::<SendTask>(handle);
            }
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            envo: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::Back => EventReply::Transition(TREE.1),
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

    #[tokio::test]
    async fn task_spawns_on_enter_and_posts_back() {
        let mut machine = Machine::new(
            &RUNNER,
            &BEHAVIORS,
            Context {
                logs: Vec::new(),
                running_task: None,
            },
        );
        let (_, idle, working) = &*TREE;
        machine.init(*idle);
        assert_eq!(machine.current(), *idle);

        // Go → working, on_enter stages TaskOp
        machine.post(Ev::Go);
        machine.advance();
        assert_eq!(machine.current(), *working);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        for _ in 0..8 {
            machine.advance();
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

    #[tokio::test]
    async fn task_started_on_enter_is_aborted_on_exit() {
        let mut machine = Machine::new(
            &RUNNER,
            &BEHAVIORS,
            Context {
                logs: Vec::new(),
                running_task: None,
            },
        );
        let (_, idle, working) = &*TREE;
        machine.init(*idle);
        assert_eq!(machine.current(), *idle);

        machine.post(Ev::Go);
        machine.advance();
        assert_eq!(machine.current(), *working);

        machine.post(Ev::Back);
        machine.advance();
        assert_eq!(machine.current(), *idle);

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        for _ in 0..4 {
            machine.advance();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        assert_eq!(machine.current(), *idle);
        assert_eq!(
            machine.context().logs,
            vec![
                "enter:idle",
                "exit:idle",
                "enter:working",
                "exit:working",
                "enter:idle"
            ]
        );
    }
}
