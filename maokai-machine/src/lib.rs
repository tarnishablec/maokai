#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::any::TypeId;
use core::cell::{Ref, RefCell, RefMut};
use core::marker::PhantomData;
use downcast::Downcast;
use maokai_gears::ops::event::{EventOp, EventOpConsumer, SharedEventQueue};
#[cfg(feature = "tokio-local-task")]
use maokai_gears::ops::task::runtimes::tokio_local::{LocalTask, TokioLocalTaskConsumer};
#[cfg(feature = "tokio-mt-task")]
use maokai_gears::ops::task::runtimes::tokio_mt::{
    SendTask, SendTaskMailboxSender, TokioMtTaskConsumer,
};
use maokai_gears::ops::task::{StartTaskOp, StopTaskOp, TaskHandle};
use maokai_reconciler::{
    IncomingDisposition, OpConsumer, OpFlow, Operation, PipelineFlow, Reconciler, Rule, RuleAccess,
    RuleResult, Ticket,
};
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

    pub fn stage<O>(&self, op: O) -> Option<Ticket>
    where
        O: Operation + 'static,
    {
        self.reconciler.borrow_mut().stage_boxed(Box::new(op), None)
    }
}

impl<E: 'static> MachineHandle<E> {
    pub fn post(&self, event: E) -> Option<Ticket> {
        self.stage(EventOp::Emit(event))
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

    pub fn stop_task(&self, handle: TaskHandle) {
        self.machine.stage(StopTaskOp(handle));
    }
}

#[cfg(feature = "tokio-mt-task")]
pub struct SendTaskSpawner<E, Context> {
    context: Shared<Context>,
    machine: MachineHandle<E>,
}

#[cfg(feature = "tokio-mt-task")]
impl<E, Context> SendTaskSpawner<E, Context> {
    fn new(context: Shared<Context>, machine: MachineHandle<E>) -> Self {
        Self { context, machine }
    }
}

#[cfg(feature = "tokio-mt-task")]
pub struct SendMachineHandle<E> {
    sender: SendTaskMailboxSender,
    _marker: PhantomData<fn(E)>,
}

#[cfg(feature = "tokio-mt-task")]
impl<E> Clone for SendMachineHandle<E> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            _marker: PhantomData,
        }
    }
}

#[cfg(feature = "tokio-mt-task")]
impl<E> SendMachineHandle<E> {
    pub fn stage<O>(&self, op: O)
    where
        O: Operation + Send + 'static,
    {
        let _ = self.sender.send(Box::new(op));
    }
}

#[cfg(feature = "tokio-mt-task")]
impl<E: Send + 'static> SendMachineHandle<E> {
    pub fn post(&self, event: E) {
        self.stage(EventOp::Emit(event));
    }
}

#[cfg(feature = "tokio-mt-task")]
pub struct SendEnvelope<E, Context> {
    context: RefCell<Context>,
    pub machine: SendMachineHandle<E>,
}

#[cfg(feature = "tokio-mt-task")]
impl<E, Context: Clone> Clone for SendEnvelope<E, Context> {
    fn clone(&self) -> Self {
        Self {
            context: RefCell::new(self.context.borrow().clone()),
            machine: self.machine.clone(),
        }
    }
}

#[cfg(feature = "tokio-mt-task")]
impl<E, Context> SendEnvelope<E, Context> {
    pub fn context(&self) -> Ref<'_, Context> {
        self.context.borrow()
    }

    pub fn context_mut(&self) -> RefMut<'_, Context> {
        self.context.borrow_mut()
    }
}

#[cfg(feature = "tokio-mt-task")]
impl<E, Context: Clone + Send + 'static> Envelope<E, Context> {
    pub fn send(&self) -> SendTaskSpawner<E, Context> {
        SendTaskSpawner::new(self.context.clone(), self.machine.clone())
    }
}

/// Priority for a transition request. Higher values win arbitration.
/// Aligned with [`Ticket`] priority (`u32`) so the same
/// number can flow through both layers without conversion.
pub type TransitionPriority = u32;

pub mod priority {
    use super::TransitionPriority;
    pub const LOW: TransitionPriority = 0;
    pub const NORMAL: TransitionPriority = 100;
    pub const HIGH: TransitionPriority = 200;
    pub const CRITICAL: TransitionPriority = 1000;
}

#[derive(Debug, Clone, Copy)]
pub struct RequestTransitionOp {
    pub target: State,
    pub priority: TransitionPriority,
}

impl RequestTransitionOp {
    pub fn new(target: State) -> Self {
        Self {
            target,
            priority: priority::NORMAL,
        }
    }

    pub fn with_priority(target: State, priority: TransitionPriority) -> Self {
        Self { target, priority }
    }
}

impl Operation for RequestTransitionOp {}

/// Slot holding the rule-arbitrated transition request. Filled by
/// `RequestTransitionConsumer` during commit and taken by the machine's
/// advance loop each microstep.
type PendingTransition = Shared<Option<RequestTransitionOp>>;

/// Decides which `RequestTransitionOp` survives when two are pending at the
/// same time. Plug-in point for custom arbitration policies.
pub trait TransitionArbiter: 'static {
    /// Return `true` if `incoming` should replace `existing`.
    fn prefer(&self, incoming: &RequestTransitionOp, existing: &RequestTransitionOp) -> bool;
}

/// Default arbiter: incoming wins when its priority is `>=` existing.
/// Equal priorities → last-writer-wins.
#[derive(Default)]
pub struct PriorityArbiter;

impl TransitionArbiter for PriorityArbiter {
    fn prefer(&self, incoming: &RequestTransitionOp, existing: &RequestTransitionOp) -> bool {
        incoming.priority >= existing.priority
    }
}

/// Reconciler rule that keeps at most one `RequestTransitionOp` in the pending
/// pool by consulting a [`TransitionArbiter`] at stage time.
pub struct RequestTransitionRule {
    arbiter: Box<dyn TransitionArbiter>,
}

impl RequestTransitionRule {
    pub fn new<A: TransitionArbiter>(arbiter: A) -> Self {
        Self {
            arbiter: Box::new(arbiter),
        }
    }
}

impl Rule for RequestTransitionRule {
    fn apply(
        &self,
        _incoming_ticket: Ticket,
        incoming: &mut dyn Operation,
        ctx: &mut dyn RuleAccess,
    ) -> RuleResult {
        let Ok(&incoming_req) = Downcast::<RequestTransitionOp>::downcast_ref(incoming) else {
            return (PipelineFlow::Continue, IncomingDisposition::Keep);
        };

        // Invariant: this rule keeps at most one RequestTransitionOp in pending.
        let existing = ctx.iter().find_map(|(t, op)| {
            Downcast::<RequestTransitionOp>::downcast_ref(op)
                .ok()
                .map(|r| (t, *r))
        });

        match existing {
            None => (PipelineFlow::Continue, IncomingDisposition::Keep),
            Some((t, e)) => {
                if self.arbiter.prefer(&incoming_req, &e) {
                    ctx.unstage(t);
                    (PipelineFlow::Continue, IncomingDisposition::Keep)
                } else {
                    (PipelineFlow::Break, IncomingDisposition::Drop)
                }
            }
        }
    }
}

pub struct RequestTransitionConsumer {
    pending: PendingTransition,
}

impl RequestTransitionConsumer {
    pub fn new(pending: PendingTransition) -> Self {
        Self { pending }
    }
}

impl OpConsumer for RequestTransitionConsumer {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<RequestTransitionOp>::downcast(op) {
            // Arbitration already done by `RequestTransitionRule`; this is just transport.
            Ok(req) => {
                *self.pending.borrow_mut() = Some(*req);
                OpFlow::Consumed
            }
            Err(err) => OpFlow::Continue(err.into_object()),
        }
    }
}

pub trait ConsumerOpList {
    fn type_ids() -> Vec<TypeId>;
}

macro_rules! impl_consumer_ops_tuple {
    ($($name:ident),+ $(,)?) => {
        impl<$($name),+> ConsumerOpList for ($($name,)+)
        where
            $($name: Operation + 'static),+
        {
            fn type_ids() -> Vec<TypeId> {
                alloc::vec![$(TypeId::of::<$name>()),+]
            }
        }
    };
}

impl_consumer_ops_tuple!(A);
impl_consumer_ops_tuple!(A, B);
impl_consumer_ops_tuple!(A, B, C);
impl_consumer_ops_tuple!(A, B, C, D);
impl_consumer_ops_tuple!(A, B, C, D, E);
impl_consumer_ops_tuple!(A, B, C, D, E, F);
impl_consumer_ops_tuple!(A, B, C, D, E, F, G);
impl_consumer_ops_tuple!(A, B, C, D, E, F, G, H);

type SharedConsumer = Rc<RefCell<dyn OpConsumer>>;

#[derive(Default)]
struct ConsumerStore {
    slots: Vec<Option<SharedConsumer>>,
    free: Vec<usize>,
}

impl ConsumerStore {
    fn insert(&mut self, c: SharedConsumer) -> usize {
        if let Some(idx) = self.free.pop() {
            self.slots[idx] = Some(c);
            idx
        } else {
            self.slots.push(Some(c));
            self.slots.len() - 1
        }
    }

    fn remove(&mut self, idx: usize) -> Option<SharedConsumer> {
        let slot = self.slots.get_mut(idx)?;
        let taken = slot.take();
        if taken.is_some() {
            self.free.push(idx);
        }
        taken
    }

    fn get(&self, idx: usize) -> Option<&SharedConsumer> {
        self.slots.get(idx).and_then(|s| s.as_ref())
    }

    fn iter(&self) -> impl Iterator<Item = &SharedConsumer> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }

    fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
    }
}

pub struct Machine<'a, 'b, T, E, Context> {
    runner: &'a Runner<'a, T>,
    behaviors: &'b Behaviors<'b, E, Envelope<E, Context>>,
    current: State,
    context: Shared<Context>,
    reconciler: Shared<Reconciler>,
    ready_events: SharedEventQueue<E>,
    pending_transitions: PendingTransition,
    consumers: ConsumerStore,
    routes: BTreeMap<TypeId, Vec<usize>>,
}

impl<'a, 'b, T, E: 'static, Context> Machine<'a, 'b, T, E, Context> {
    fn install_default_rules(&mut self) {
        self.reconciler
            .borrow_mut()
            .add_rule(RequestTransitionRule::new(PriorityArbiter));
    }

    fn install_default_consumers(&mut self) {
        self.set_consumer::<(EventOp<E>,), _>(EventOpConsumer::<E>::new(self.ready_events.clone()));
        self.set_consumer::<(RequestTransitionOp,), _>(RequestTransitionConsumer::new(
            self.pending_transitions.clone(),
        ));
        #[cfg(feature = "tokio-local-task")]
        {
            self.set_consumer::<(StartTaskOp<LocalTask>, StopTaskOp), _>(
                TokioLocalTaskConsumer::default(),
            );
        }
        #[cfg(feature = "tokio-mt-task")]
        {
            self.set_consumer::<(StartTaskOp<SendTask>, StopTaskOp), _>(
                TokioMtTaskConsumer::default(),
            );
        }
    }

    pub fn new(
        runner: &'a Runner<'a, T>,
        behaviors: &'b Behaviors<'b, E, Envelope<E, Context>>,
        context: Context,
    ) -> Self {
        let ready_events = Rc::new(RefCell::new(VecDeque::new()));
        let pending_transitions = Rc::new(RefCell::new(None));
        let mut machine = Self {
            runner,
            behaviors,
            current: runner.tree.nil(),
            context: Rc::new(RefCell::new(context)),
            reconciler: Rc::new(RefCell::new(Reconciler::default())),
            ready_events,
            pending_transitions,
            consumers: ConsumerStore::default(),
            routes: BTreeMap::new(),
        };
        machine.install_default_rules();
        machine.install_default_consumers();
        machine
    }

    /// Replace the transition arbiter. Subsequent `RequestTransitionOp`s use the
    /// new policy; any already-pending request is unaffected.
    pub fn set_transition_arbiter<A: TransitionArbiter>(&mut self, arbiter: A) {
        self.reconciler
            .borrow_mut()
            .add_rule(RequestTransitionRule::new(arbiter));
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

    pub fn set_consumer<Ops, Cn>(&mut self, consumer: Cn)
    where
        Ops: ConsumerOpList,
        Cn: OpConsumer + 'static,
    {
        let consumer: SharedConsumer = Rc::new(RefCell::new(consumer));
        let idx = self.consumers.insert(consumer);
        for tid in Ops::type_ids() {
            self.routes.entry(tid).or_default().push(idx);
        }
    }

    pub fn with_consumer<Ops, Cn>(mut self, consumer: Cn) -> Self
    where
        Ops: ConsumerOpList,
        Cn: OpConsumer + 'static,
    {
        self.set_consumer::<Ops, Cn>(consumer);
        self
    }

    pub fn remove_consumer<Ops>(&mut self) -> Option<Vec<SharedConsumer>>
    where
        Ops: ConsumerOpList,
    {
        use alloc::collections::BTreeSet;

        let mut affected: BTreeSet<usize> = BTreeSet::new();
        for tid in Ops::type_ids() {
            if let Some(indices) = self.routes.remove(&tid) {
                for idx in indices {
                    affected.insert(idx);
                }
            }
        }
        if affected.is_empty() {
            return None;
        }

        // GC: only drop storage entries no longer referenced by any remaining route.
        let still_referenced: BTreeSet<usize> = self
            .routes
            .values()
            .flat_map(|v| v.iter().copied())
            .collect();

        let mut removed = Vec::new();
        for idx in affected {
            if !still_referenced.contains(&idx)
                && let Some(c) = self.consumers.remove(idx)
            {
                removed.push(c);
            }
        }

        if removed.is_empty() {
            None
        } else {
            Some(removed)
        }
    }

    pub fn clear_consumers(&mut self) {
        self.consumers.clear();
        self.routes.clear();
        self.install_default_consumers();
    }
}

#[cfg(feature = "tokio-local-task")]
pub struct LocalTaskSpawner<E, Context> {
    context: Shared<Context>,
    machine: MachineHandle<E>,
}

#[cfg(feature = "tokio-local-task")]
impl<E, Context> LocalTaskSpawner<E, Context> {
    fn new(context: Shared<Context>, machine: MachineHandle<E>) -> Self {
        Self { context, machine }
    }
}

#[cfg(feature = "tokio-local-task")]
impl<E: 'static, Context: 'static> Envelope<E, Context> {
    pub fn local(&self) -> LocalTaskSpawner<E, Context> {
        LocalTaskSpawner::new(self.context.clone(), self.machine.clone())
    }
}

#[cfg(feature = "tokio-local-task")]
impl<E: 'static, Context: 'static> LocalTaskSpawner<E, Context> {
    pub fn start_task<F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(Envelope<E, Context>) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let handle = TaskHandle::next();
        let envelope = Envelope {
            context: self.context.clone(),
            machine: self.machine.clone(),
        };
        let task: LocalTask = Box::new(move |_| {
            Box::pin(build(envelope)) as core::pin::Pin<Box<dyn Future<Output = ()> + 'static>>
        });
        self.machine.stage(StartTaskOp { handle, task });
        handle
    }
}

#[cfg(feature = "tokio-mt-task")]
impl<E, Context: Clone + Send + 'static> SendTaskSpawner<E, Context> {
    pub fn start_task<F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(SendEnvelope<E, Context>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handle = TaskHandle::next();
        let ctx = self.context.borrow().clone();
        let task: SendTask = Box::new(move |emitter| {
            let envelope = SendEnvelope {
                context: RefCell::new(ctx),
                machine: SendMachineHandle {
                    sender: emitter.into_sender(),
                    _marker: PhantomData,
                },
            };
            Box::pin(build(envelope))
                as core::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        });
        self.machine.stage(StartTaskOp { handle, task });
        handle
    }
}

#[cfg(feature = "tokio-mt-task")]
impl<E, Context: Clone + Send + 'static> SendEnvelope<E, Context> {
    pub fn start_task<F, Fut>(&self, build: F) -> TaskHandle
    where
        F: FnOnce(SendEnvelope<E, Context>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handle = TaskHandle::next();
        let ctx = self.context.borrow().clone();
        let task: SendTask = Box::new(move |emitter| {
            let envelope = SendEnvelope {
                context: RefCell::new(ctx),
                machine: SendMachineHandle {
                    sender: emitter.into_sender(),
                    _marker: PhantomData,
                },
            };
            Box::pin(build(envelope))
                as core::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        });
        self.machine.stage(StartTaskOp { handle, task });
        handle
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
            .stage(RequestTransitionOp::new(target));
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
                    'route: loop {
                        let tid = (&*op as &dyn core::any::Any).type_id();

                        let Some(indices) = self.routes.get(&tid) else {
                            on_unhandled(ticket, op);
                            return;
                        };

                        let mut next_consumer = 0usize;
                        loop {
                            let Some(&idx) = indices.get(next_consumer) else {
                                on_unhandled(ticket, op);
                                return;
                            };
                            next_consumer += 1;

                            let Some(consumer) = self.consumers.get(idx).cloned() else {
                                continue;
                            };

                            match consumer.borrow_mut().consume(ticket, op) {
                                OpFlow::Consumed => return,
                                OpFlow::Continue(next) => {
                                    let next_tid = (&*next as &dyn core::any::Any).type_id();
                                    op = next;
                                    if next_tid != tid {
                                        continue 'route;
                                    }
                                }
                            }
                        }
                    }
                });
            }

            let mut drained_any = false;
            for consumer in self.consumers.iter() {
                if consumer
                    .borrow_mut()
                    .drain(&mut self.reconciler.borrow_mut())
                {
                    drained_any = true;
                }
            }

            if drained_any {
                continue;
            }

            // Apply at most one arbitrated transition per microstep. Consumer already
            // picked the winner; any losers were dropped at arbitration time.
            if let Some(req) = self.pending_transitions.borrow_mut().take() {
                progressed = true;
                let envo = self.envelope();
                self.current =
                    self.runner
                        .transition(self.behaviors, &self.current, &req.target, envo);
            }

            while let Some(event) = self.ready_events.borrow_mut().pop_front() {
                progressed = true;
                // Dispatch runs `on_event` which can stage further ops (including
                // `RequestTransitionOp`); those will be arbitrated next microstep.
                let _ =
                    self.runner
                        .dispatch(self.behaviors, &self.current, &event, self.envelope());
            }

            if !progressed
                && !self.reconciler.borrow().has_pending()
                && self.ready_events.borrow().is_empty()
                && self.pending_transitions.borrow().is_none()
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
            envo: Envelope<LightEvent, Context>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, _, opened, _) = &*SETUP_TREE;
            match event {
                LightEvent::Open => {
                    envo.machine.stage(RequestTransitionOp::new(*opened));
                    EventReply::Handled
                }
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
            envo: Envelope<LightEvent, Context>,
            _tree: &dyn TreeView,
        ) -> EventReply {
            let (_, closed, _, shining) = &*SETUP_TREE;

            match event {
                LightEvent::Close => {
                    envo.machine.stage(RequestTransitionOp::new(*closed));
                    EventReply::Handled
                }
                LightEvent::Shine => {
                    envo.machine.stage(RequestTransitionOp::new(*shining));
                    EventReply::Handled
                }
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

#[cfg(all(test, feature = "tokio-local-task"))]
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
            envo: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            match event {
                Ev::Go => {
                    envo.machine.stage(RequestTransitionOp::new(TREE.2));
                    EventReply::Handled
                }
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<Ev, Envelope<Ev, Context>> for WorkingBehavior {
        fn on_enter(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context.borrow_mut().logs.push("enter:working");
            let _ = envo.local().start_task(|envo| async move {
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
                    envo.machine.stage(RequestTransitionOp::new(TREE.1));
                    EventReply::Handled
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

                // Go -> working, on_enter stages a `start-task` op.
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

#[cfg(all(test, feature = "tokio-mt-task"))]
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
            envo: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            let (_, _, working) = &*TREE;
            match event {
                Ev::Go => {
                    envo.machine.stage(RequestTransitionOp::new(*working));
                    EventReply::Handled
                }
                _ => EventReply::Ignored,
            }
        }
    }

    impl Behavior<Ev, Envelope<Ev, Context>> for WorkingBehavior {
        fn on_enter(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context_mut().logs.push("enter:working");
            let handle = envo.send().start_task(|envo| async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                envo.machine.post(Ev::TaskDone);
            });
            envo.context_mut().running_task = Some(handle);
        }
        fn on_exit(&self, _: &Transition, envo: Envelope<Ev, Context>) {
            envo.context_mut().logs.push("exit:working");
            if let Some(handle) = envo.context_mut().running_task.take() {
                envo.stop_task(handle);
            }
        }
        fn on_event(
            &self,
            event: &Ev,
            _: &State,
            envo: Envelope<Ev, Context>,
            _: &dyn TreeView,
        ) -> EventReply {
            let (_, idle, _) = &*TREE;
            match event {
                Ev::Back => {
                    envo.machine.stage(RequestTransitionOp::new(*idle));
                    EventReply::Handled
                }
                Ev::TaskDone => {
                    envo.context.borrow_mut().logs.push("task:done");
                    envo.machine.stage(RequestTransitionOp::new(*idle));
                    EventReply::Handled
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

        // Go -> working, on_enter stages a `start-task` op.
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
