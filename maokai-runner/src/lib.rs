#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use maokai_tree::{State, StateTree, TreeView};

// --- Core Runner Logic ---

/// Defines the result of an event dispatch.
pub enum EventReply {
    /// Event was handled; do not bubble up.
    Handled,
    /// Did not handle the event; bubble it to the parent.
    Ignored,
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
pub trait Behavior<E, Context = ()>: Send + Sync {
    /// Called when the state machine enters this state.
    fn on_enter(&self, _transition: &Transition, _context: Context) {}
    /// Called when the state machine exits this state.
    fn on_exit(&self, _transition: &Transition, _context: Context) {}
    /// Processes an incoming event.
    fn on_event(
        &self,
        _event: &E,
        _current: &State,
        _context: Context,
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
        context: Context,
    ) -> State
    where
        Context: Clone,
    {
        let (exit_list, enter_list) = self.tree.propose_transition(current, target);
        let transition = Transition {
            from: *current,
            target: *target,
            exit_list,
            enter_list,
        };

        // Notify states being exited (from leaf towards LCA)
        for state in &transition.exit_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_exit(&transition, context.clone());
            }
        }

        // Notify states being entered (from LCA towards target leaf)
        for state in &transition.enter_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_enter(&transition, context.clone());
            }
        }

        *target
    }
}

impl<'a, T> Runner<'a, T>
where
    StateTree<T>: TreeView,
{
    /// Dispatches an event starting from `current`, bubbling to the parent until a
    /// behavior handles it. Transitions are no longer triggered inline — behaviors
    /// produce them as side effects (e.g. staging `RequestTransitionOp`).
    pub fn dispatch<E, Context>(
        &self,
        behaviors: &Behaviors<E, Context>,
        current: &State,
        event: &E,
        context: Context,
    ) -> EventReply
    where
        Context: Clone,
    {
        let mut probe = *current;
        loop {
            if let Some(behavior) = behaviors.map.get(&probe) {
                match behavior.on_event(event, current, context.clone(), self.tree) {
                    EventReply::Handled => return EventReply::Handled,
                    EventReply::Ignored => {}
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

    impl<'a, Context> Behavior<Event, Context> for BlinkyBehavior<'a> {
        fn on_enter(&self, transition: &Transition, _ctx: Context) {
            self.log.push("enter", transition);
        }
        fn on_exit(&self, transition: &Transition, _ctx: Context) {
            self.log.push("exit", transition);
        }
        fn on_event(&self, _e: &Event, _c: &State, _ctx: Context, _t: &dyn TreeView) -> EventReply {
            EventReply::Handled
        }
    }

    impl Behavior<Event, Context> for CountingBehavior {
        fn on_enter(&self, _transition: &Transition, ctx: Context) {
            ctx.borrow_mut().enters += 1;
        }

        fn on_exit(&self, _transition: &Transition, ctx: Context) {
            ctx.borrow_mut().exits += 1;
        }

        fn on_event(&self, _e: &Event, _c: &State, ctx: Context, _t: &dyn TreeView) -> EventReply {
            ctx.borrow_mut().events += 1;
            EventReply::Handled
        }
    }

    #[test]
    fn transition_fires_exit_then_enter() {
        let (tree, off, on) = build_tree();

        let log = Log::default();
        let mut behaviors = Behaviors::default();
        behaviors.register(
            &off,
            BlinkyBehavior { log: &log },
        );
        behaviors.register(
            &on,
            BlinkyBehavior { log: &log },
        );

        let runner = Runner::new(&tree);

        runner.transition(&behaviors, &off, &on, ());
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

        runner.transition(&behaviors, &on, &off, ());
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
        behaviors.register(
            &off,
            BlinkyBehavior { log: &log },
        );

        let runner = Runner::new(&tree);
        runner.transition(&behaviors, &off, &off, ());

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
        let ctx = Rc::new(RefCell::new(StateContext::default()));

        let reply = runner.dispatch(&behaviors, &off, &Event::Toggle, ctx.clone());

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
    fn dispatch_ignored_when_no_behavior_handles() {
        let (tree, _, _) = build_tree();
        let behaviors: Behaviors<Event> = Behaviors::default();
        let runner = Runner::new(&tree);

        let reply = runner.dispatch(&behaviors, &tree.root(), &Event::Toggle, ());
        assert!(matches!(reply, EventReply::Ignored));
    }
}
