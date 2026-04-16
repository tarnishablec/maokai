#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use maokai_tree::{State, StateTree, TreeView};

// --- Core Runner Logic ---

/// Defines the result of an event dispatch.
///
/// Used both as the per-behavior reply from `on_event` and as the aggregate
/// outcome of `Runner::dispatch` (which returns the first non-`Ignored` reply
/// produced along the bubble chain, or `Ignored` if nothing handled it).
pub enum EventReply {
    /// Event was handled; do not bubble up.
    Handled,
    /// Did not handle the event; bubble it to the parent.
    Ignored,
    /// Event was handled; do not bubble up, and request a transition to `target`.
    /// The runner does not apply the transition itself — callers inspect the
    /// dispatch result and either invoke `Runner::transition` (standalone use)
    /// or stage a `RequestTransitionOp` (e.g. `maokai-machine` for arbitration).
    Transition(State),
}

/// Describes a transition from one state to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: State,
    pub target: State,
    pub exit_list: Vec<State>,
    pub enter_list: Vec<State>,
}

// pub trait Context {}
//
// impl<T> Context for T {}

/// Defines the life-cycle and event-handling logic for a specific state.
///
/// Context is borrowed exclusively. Behaviors that only need shared access
/// can reborrow internally (`&*ctx`); handle-style contexts (e.g., an
/// `Rc`-backed envelope) remain cheap to thread through because their own
/// methods take `&self` and auto-reborrow from `&mut`.
pub trait Behavior<E, Context = ()>: Send + Sync {
    fn on_enter(&self, _transition: &Transition, _context: &mut Context) {}
    fn on_exit(&self, _transition: &Transition, _context: &mut Context) {}
    fn on_event(
        &self,
        _event: &E,
        _current: &State,
        _context: &mut Context,
        _tree: &dyn TreeView,
    ) -> EventReply {
        EventReply::Handled
    }
}

/// A registry mapping `State` handles to their respective `Behavior` implementations.
pub struct Behaviors<'a, E, Context = ()> {
    map: BTreeMap<State, Box<dyn Behavior<E, Context> + 'a>>,
}

impl<E, Context> Default for Behaviors<'_, E, Context> {
    fn default() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }
}

impl<'a, E, Context> Behaviors<'a, E, Context> {
    /// Registers a behavior for a specific state.
    pub fn register(&mut self, state: &State, behavior: impl Behavior<E, Context> + 'a) {
        self.map.insert(*state, Box::new(behavior));
    }
}

/// The execution engine that manages state transitions and event bubbling.
pub struct Runner<'a, T> {
    pub tree: &'a StateTree<T>,
}

impl<'a, T> Runner<'a, T> {
    pub fn new(tree: &'a StateTree<T>) -> Self {
        Self { tree }
    }

    /// Executes a transition sequence: calls `on_exit` for all exiting states,
    /// followed by `on_enter` for all entering states.
    pub fn transition<E, Context>(
        &self,
        behaviors: &Behaviors<E, Context>,
        current: &State,
        target: &State,
        context: &mut Context,
    ) -> State {
        let (exit_list, enter_list) = self.tree.propose_transition(current, target);
        let transition = Transition {
            from: *current,
            target: *target,
            exit_list,
            enter_list,
        };

        for state in &transition.exit_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_exit(&transition, context);
            }
        }

        for state in &transition.enter_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_enter(&transition, context);
            }
        }

        *target
    }
}

impl<'a, T> Runner<'a, T>
where
    StateTree<T>: TreeView,
{
    /// Dispatches an event starting from `current`, bubbling toward the root
    /// until a behavior returns a non-`Ignored` reply. The winning reply is
    /// returned verbatim; `Ignored` is returned only when no level handled it.
    ///
    /// `dispatch` never applies a `Transition(target)` inline — it only reports
    /// intent. Callers decide how to act on it (execute via `transition`, or
    /// stage for arbitration).
    pub fn dispatch<E, Context>(
        &self,
        behaviors: &Behaviors<E, Context>,
        current: &State,
        event: &E,
        context: &mut Context,
    ) -> EventReply {
        let mut probe = *current;
        loop {
            if let Some(behavior) = behaviors.map.get(&probe) {
                match behavior.on_event(event, current, context, self.tree) {
                    EventReply::Ignored => {}
                    other => return other,
                }
            }
            match self.tree.parent_of(&probe) {
                Some(parent) => probe = parent,
                None => return EventReply::Ignored,
            }
        }
    }
}

// --- Tests with State Validation ---

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    extern crate std;
    use super::*;
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use std::sync::Mutex;

    #[derive(Debug)]
    enum Event {
        Toggle,
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct StateContext {
        events: usize,
        exits: usize,
        enters: usize,
    }

    type Context = Rc<RefCell<StateContext>>;

    /// Thread-safe log to track call sequences and the specific State handles passed.
    #[derive(Default)]
    struct Log {
        entries: Mutex<Vec<(&'static str, Transition)>>,
    }

    impl Log {
        fn push(&self, action: &'static str, transition: &Transition) {
            self.entries
                .lock()
                .unwrap()
                .push((action, transition.clone()));
        }
        fn take(&self) -> Vec<(&'static str, Transition)> {
            core::mem::take(&mut *self.entries.lock().unwrap())
        }
    }

    struct BlinkyBehavior<'a> {
        log: &'a Log,
    }

    struct CountingBehavior;

    fn build_tree() -> (StateTree<&'static str>, State, State) {
        let mut tree = StateTree::new("root");
        let off = tree.add_child(&tree.root(), "off");
        let on = tree.add_child(&tree.root(), "on");
        (tree, off, on)
    }

    impl<'a, Ctx> Behavior<Event, Ctx> for BlinkyBehavior<'a> {
        fn on_enter(&self, transition: &Transition, _ctx: &mut Ctx) {
            self.log.push("enter", transition);
        }
        fn on_exit(&self, transition: &Transition, _ctx: &mut Ctx) {
            self.log.push("exit", transition);
        }
        fn on_event(
            &self,
            _e: &Event,
            _c: &State,
            _ctx: &mut Ctx,
            _t: &dyn TreeView,
        ) -> EventReply {
            EventReply::Handled
        }
    }

    impl Behavior<Event, Context> for CountingBehavior {
        fn on_enter(&self, _transition: &Transition, ctx: &mut Context) {
            ctx.borrow_mut().enters += 1;
        }

        fn on_exit(&self, _transition: &Transition, ctx: &mut Context) {
            ctx.borrow_mut().exits += 1;
        }

        fn on_event(
            &self,
            _e: &Event,
            _c: &State,
            ctx: &mut Context,
            _t: &dyn TreeView,
        ) -> EventReply {
            ctx.borrow_mut().events += 1;
            EventReply::Handled
        }
    }

    #[test]
    fn transition_fires_exit_then_enter() {
        let (tree, off, on) = build_tree();

        let log = Log::default();
        let mut behaviors = Behaviors::default();
        behaviors.register(&off, BlinkyBehavior { log: &log });
        behaviors.register(&on, BlinkyBehavior { log: &log });

        let runner = Runner::new(&tree);

        runner.transition(&behaviors, &off, &on, &mut ());
        let results = log.take();
        let expected = Transition {
            from: off,
            target: on,
            exit_list: alloc::vec![off],
            enter_list: alloc::vec![on],
        };
        assert_eq!(
            results,
            [("exit", expected.clone()), ("enter", expected.clone())]
        );

        runner.transition(&behaviors, &on, &off, &mut ());
        let results = log.take();
        let expected = Transition {
            from: on,
            target: off,
            exit_list: alloc::vec![on],
            enter_list: alloc::vec![off],
        };
        assert_eq!(
            results,
            [("exit", expected.clone()), ("enter", expected.clone())]
        );
    }

    #[test]
    fn self_transition_exit_and_enter_same_state() {
        let (tree, off, _) = build_tree();
        let log = Log::default();
        let mut behaviors = Behaviors::default();
        behaviors.register(&off, BlinkyBehavior { log: &log });

        let runner = Runner::new(&tree);
        runner.transition(&behaviors, &off, &off, &mut ());

        let results = log.take();
        let expected = Transition {
            from: off,
            target: off,
            exit_list: alloc::vec![off],
            enter_list: alloc::vec![off],
        };
        assert_eq!(results, [("exit", expected.clone()), ("enter", expected)]);
    }

    #[test]
    fn dispatch_bubbles_until_handled() {
        let (tree, off, _on) = build_tree();

        let mut behaviors = Behaviors::default();
        behaviors.register(&off, CountingBehavior);

        let runner = Runner::new(&tree);
        let mut ctx = Rc::new(RefCell::new(StateContext::default()));

        let reply = runner.dispatch(&behaviors, &off, &Event::Toggle, &mut ctx);

        assert!(matches!(reply, EventReply::Handled));
        assert_eq!(
            *ctx.borrow(),
            StateContext {
                events: 1,
                exits: 0,
                enters: 0,
            }
        );
    }

    #[test]
    fn dispatch_returns_transition_intent_without_applying() {
        let (tree, off, on) = build_tree();
        let log = Log::default();

        struct RequestingBehavior {
            target: State,
        }
        impl<Ctx> Behavior<Event, Ctx> for RequestingBehavior {
            fn on_event(
                &self,
                _e: &Event,
                _c: &State,
                _ctx: &mut Ctx,
                _t: &dyn TreeView,
            ) -> EventReply {
                EventReply::Transition(self.target)
            }
        }

        let mut behaviors = Behaviors::default();
        behaviors.register(&off, RequestingBehavior { target: on });
        // Registered on the target to detect any accidental inline applied.
        behaviors.register(&on, BlinkyBehavior { log: &log });

        let runner = Runner::new(&tree);
        let reply = runner.dispatch(&behaviors, &off, &Event::Toggle, &mut ());

        assert!(matches!(reply, EventReply::Transition(t) if t == on));
        assert!(log.take().is_empty());
    }

    #[test]
    fn dispatch_bubbles_past_ignored_to_transition_at_parent() {
        let mut tree = StateTree::new("root");
        let parent = tree.add_child(&tree.root(), "parent");
        let leaf = tree.add_child(&parent, "leaf");

        struct BubbleBehavior;
        impl<Ctx> Behavior<Event, Ctx> for BubbleBehavior {
            fn on_event(
                &self,
                _e: &Event,
                _c: &State,
                _ctx: &mut Ctx,
                _t: &dyn TreeView,
            ) -> EventReply {
                EventReply::Ignored
            }
        }
        struct RequestingBehavior {
            target: State,
        }
        impl<Ctx> Behavior<Event, Ctx> for RequestingBehavior {
            fn on_event(
                &self,
                _e: &Event,
                _c: &State,
                _ctx: &mut Ctx,
                _t: &dyn TreeView,
            ) -> EventReply {
                EventReply::Transition(self.target)
            }
        }

        let mut behaviors = Behaviors::default();
        behaviors.register(&leaf, BubbleBehavior);
        behaviors.register(&parent, RequestingBehavior { target: leaf });

        let runner = Runner::new(&tree);
        let reply = runner.dispatch(&behaviors, &leaf, &Event::Toggle, &mut ());
        assert!(matches!(reply, EventReply::Transition(t) if t == leaf));
    }

    #[test]
    fn dispatch_ignored_when_no_behavior_handles() {
        let (tree, _, _) = build_tree();
        let behaviors: Behaviors<Event> = Behaviors::default();
        let runner = Runner::new(&tree);

        let reply = runner.dispatch(&behaviors, &tree.root(), &Event::Toggle, &mut ());
        assert!(matches!(reply, EventReply::Ignored));
    }

    /// Exclusive context mutation — flat state, no interior mutability.
    #[test]
    fn exclusive_context_mutation() {
        let (tree, off, on) = build_tree();

        #[derive(Default, PartialEq, Eq, Debug)]
        struct FlatState {
            enters: usize,
            exits: usize,
            events: usize,
        }

        struct ExclusiveBehavior {
            target: State,
        }
        impl Behavior<Event, FlatState> for ExclusiveBehavior {
            fn on_enter(&self, _t: &Transition, ctx: &mut FlatState) {
                ctx.enters += 1;
            }
            fn on_exit(&self, _t: &Transition, ctx: &mut FlatState) {
                ctx.exits += 1;
            }
            fn on_event(
                &self,
                _e: &Event,
                _c: &State,
                ctx: &mut FlatState,
                _tree: &dyn TreeView,
            ) -> EventReply {
                ctx.events += 1;
                EventReply::Transition(self.target)
            }
        }

        let mut behaviors = Behaviors::default();
        behaviors.register(&off, ExclusiveBehavior { target: on });
        behaviors.register(&on, ExclusiveBehavior { target: off });

        let runner = Runner::new(&tree);
        let mut state = FlatState::default();

        let reply = runner.dispatch(&behaviors, &off, &Event::Toggle, &mut state);
        assert!(matches!(reply, EventReply::Transition(t) if t == on));

        let current = if let EventReply::Transition(target) = reply {
            runner.transition(&behaviors, &off, &target, &mut state)
        } else {
            off
        };
        assert_eq!(current, on);
        assert_eq!(
            state,
            FlatState {
                enters: 1,
                exits: 1,
                events: 1
            }
        );
    }
}
