#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use maokai_tree::{State, StateTree, TreeView};

// --- Core Runner Logic ---

/// Defines the result of an event dispatch.
pub enum EventReply {
    /// Event was handled; do not bubble up or transition.
    Handled,
    /// Did not handle the event; bubble it to the parent.
    Ignored,
    /// Trigger a transition to a new target state.
    Transition(State),
}

/// Defines the life-cycle and event-handling logic for a specific state.
pub trait Behavior<E>: Send + Sync {
    /// Called when the state machine enters this state.
    fn on_enter(&self, _state: &State) {}
    /// Called when the state machine exits this state.
    fn on_exit(&self, _state: &State) {}
    /// Processes an incoming event.
    fn on_event(&self, event: &E, current: &State, tree: &dyn TreeView) -> EventReply;
}

/// A registry mapping `State` handles to their respective `Behavior` implementations.
pub struct Behaviors<'a, E> {
    map: BTreeMap<State, Box<dyn Behavior<E> + 'a>>,
}

impl<E> Default for Behaviors<'_, E> {
    fn default() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }
}

impl<'a, E> Behaviors<'a, E> {
    /// Registers a behavior for a specific state.
    pub fn register(&mut self, state: &State, behavior: impl Behavior<E> + 'a) {
        self.map.insert(state.clone(), Box::new(behavior));
    }
}

/// The execution engine that manages state transitions and event bubbling.
pub struct Runner<'a, T> {
    tree: &'a StateTree<T>,
}

impl<'a, T> Runner<'a, T> {
    pub fn new(tree: &'a StateTree<T>) -> Self {
        Self { tree }
    }

    /// Executes a transition sequence: calls `on_exit` for all exiting states,
    /// followed by `on_enter` for all entering states.
    pub fn transition<E>(
        &self,
        behaviors: &Behaviors<E>,
        current: &State,
        target: &State,
    ) -> State {
        let (exit_list, enter_list) = self.tree.propose_transition(current, target);

        // Notify states being exited (from leaf towards LCA)
        for state in &exit_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_exit(state);
            }
        }

        // Notify states being entered (from LCA towards target leaf)
        for state in &enter_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_enter(state);
            }
        }

        target.clone()
    }
}

impl<'a, T> Runner<'a, T>
where
    StateTree<T>: TreeView,
{
    /// Dispatches an event starting from the `current` state.
    /// Follows Run-to-Completion (RTC) semantics.
    pub fn dispatch<E>(&self, behaviors: &Behaviors<E>, current: &State, event: &E) -> State {
        match self.bubble(behaviors, current, event) {
            EventReply::Transition(target) => {
                self.transition(behaviors, current, &target);
                target
            }
            _ => current.clone(),
        }
    }

    /// Bubbles an event from the current state up to the root until handled or transitioned.
    fn bubble<E>(&self, behaviors: &Behaviors<E>, current: &State, event: &E) -> EventReply {
        let mut probe = current.clone();
        loop {
            if let Some(behavior) = behaviors.map.get(&probe) {
                match behavior.on_event(event, current, self.tree) {
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
    use alloc::vec::Vec;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum S {
        Root,
        Off,
        On,
    }

    #[derive(Debug)]
    enum Event {
        Toggle,
    }

    /// Thread-safe log to track call sequences and the specific State handles passed.
    #[derive(Default)]
    struct Log {
        entries: Mutex<Vec<(&'static str, State)>>,
    }

    impl Log {
        fn push(&self, action: &'static str, state: &State) {
            self.entries.lock().unwrap().push((action, state.clone()));
        }
        fn take(&self) -> Vec<(&'static str, State)> {
            core::mem::take(&mut *self.entries.lock().unwrap())
        }
    }

    struct BlinkyBehavior<'a> {
        log: &'a Log,
        target: State,
    }

    impl<'a> Behavior<Event> for BlinkyBehavior<'a> {
        fn on_enter(&self, state: &State) {
            self.log.push("enter", state);
        }
        fn on_exit(&self, state: &State) {
            self.log.push("exit", state);
        }
        fn on_event(&self, _e: &Event, _c: &State, _t: &dyn TreeView) -> EventReply {
            EventReply::Transition(self.target.clone())
        }
    }

    #[test]
    fn test_state_parameters_passed_correctly() {
        let mut tree = StateTree::new(S::Root);
        let off = tree.add_child(&tree.root(), S::Off);
        let on = tree.add_child(&tree.root(), S::On);

        let log = Log::default();
        let mut behaviors = Behaviors::default();

        // Register behaviors that point to each other
        behaviors.register(
            &off,
            BlinkyBehavior {
                log: &log,
                target: on.clone(),
            },
        );
        behaviors.register(
            &on,
            BlinkyBehavior {
                log: &log,
                target: off.clone(),
            },
        );

        let runner = Runner::new(&tree);

        // 1. Dispatch Toggle while in the 'Off' state
        runner.dispatch(&behaviors, &off, &Event::Toggle);
        let results = log.take();

        // Verify that 'exit' received the 'off' handle and 'enter' received the 'on' handle
        assert_eq!(results, [("exit", off.clone()), ("enter", on.clone())]);

        // 2. Dispatch Toggle while in the 'On' state
        runner.dispatch(&behaviors, &on, &Event::Toggle);
        let results = log.take();

        // Verify reverse transition handles
        assert_eq!(results, [("exit", on.clone()), ("enter", off.clone())]);
    }

    #[test]
    fn test_self_transition_state_parameters() {
        let mut tree = StateTree::new(S::Root);
        let off = tree.add_child(&tree.root(), S::Off);
        let log = Log::default();
        let mut behaviors = Behaviors::default();

        // Behavior that transitions to itself
        behaviors.register(
            &off,
            BlinkyBehavior {
                log: &log,
                target: off.clone(),
            },
        );

        let runner = Runner::new(&tree);
        runner.dispatch(&behaviors, &off, &Event::Toggle);

        let results = log.take();
        // In a self-transition, both exit and enter should receive the same state handle
        assert_eq!(results, [("exit", off.clone()), ("enter", off.clone())]);
    }
}
