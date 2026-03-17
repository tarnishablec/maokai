#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use maokai_tree::{State, StateTree, TreeLookup};

// -- Public Types ------------------------------------------------------------------

pub enum EventReply {
    /// Event handled, no transition
    Handled,
    /// Event not be handled, bubbles up to parent state
    Ignored,
    /// Trigger transition (allows target == current, treated as self-transition, triggers exit + enter)
    Transition(State),
}

pub trait Behavior<E> {
    fn on_enter(&mut self, _current: &State) {}
    fn on_exit(&mut self, _current: &State) {}
    fn on_event(&mut self, event: &E, current: &State, tree: &dyn TreeLookup) -> EventReply;
}

// -- Behaviors: External behavior registry (owned by caller) ------------------

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
    pub fn register(&mut self, state: &State, behaviour: impl Behavior<E> + 'a) {
        self.map.insert(state.clone(), Box::new(behaviour));
    }
}

// -- Runner: Stateless calculator ----------------------------------------------

pub struct Runner<'a, T> {
    tree: &'a StateTree<T>,
}

impl<'a, T: PartialEq + 'static> Runner<'a, T> {
    pub fn new(tree: &'a StateTree<T>) -> Self {
        Self { tree }
    }

    /// Dispatches an event, RTC (Run-To-Completion) semantics:
    /// - Bubbles up from current to ancestors, finds the first state that handles the event
    /// - If Transition is returned, executes transition sequence (exit / enter)
    /// - Returns the final state after transition
    ///
    /// Note: no re-entrant. `dispatch` should not be called within on_event / on_exit / on_enter.
    pub fn dispatch<E>(&self, behaviours: &mut Behaviors<E>, current: &State, event: &E) -> State {
        match self.bubble(behaviours, current, event) {
            EventReply::Transition(target) => {
                self.transition(behaviours, current, &target);
                target
            }
            EventReply::Handled | EventReply::Ignored => current.clone(),
        }
    }

    /// Executes transition sequence: exit_list calls on_exit, enter_list calls on_enter sequentially
    pub fn transition<E>(
        &self,
        behaviours: &mut Behaviors<E>,
        current: &State,
        target: &State,
    ) -> State {
        let (exit_list, enter_list) = self.tree.propose_transition(current, target);

        for state in &exit_list {
            if let Some(b) = behaviours.map.get_mut(state) {
                b.on_exit(current);
            }
        }

        for state in &enter_list {
            if let Some(b) = behaviours.map.get_mut(state) {
                b.on_enter(current);
            }
        }

        target.clone()
    }

    // -- Private Implementation --------------------------------------------------

    /// Bubbles up to ancestors, skips nodes without registered behavior, until root
    fn bubble<E>(&self, behaviours: &mut Behaviors<E>, current: &State, event: &E) -> EventReply {
        let mut probe = current.clone();

        loop {
            if let Some(b) = behaviours.map.get_mut(&probe) {
                match b.on_event(event, current, self.tree) {
                    EventReply::Ignored => {}
                    other => return other, // Handled or Transition
                }
            }
            match self.tree.parent_of(&probe) {
                Some(parent) => probe = parent,
                None => return EventReply::Handled, // Reached root with no handler, treated as handled (consumed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    extern crate alloc;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use maokai_tree::{TreeLookup, lookup};

    // -------------------------------------------------------------
    // State definition (enum)
    // -------------------------------------------------------------
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum BlinkyState {
        Root,
        Off,
        On,
    }

    // -------------------------------------------------------------
    // Event definition
    // -------------------------------------------------------------
    #[derive(Debug)]
    enum Event {
        Toggle,
    }

    // -------------------------------------------------------------
    // Log (used for asserting execution order)
    // -------------------------------------------------------------
    #[derive(Default)]
    struct Log {
        entries: RefCell<Vec<&'static str>>,
    }

    impl Log {
        fn push(&self, s: &'static str) {
            self.entries.borrow_mut().push(s);
        }

        fn take(&self) -> Vec<&'static str> {
            core::mem::take(&mut *self.entries.borrow_mut())
        }
    }

    // ─────────────────────────────────────────────────────────────
    // Off Behaviour
    // ─────────────────────────────────────────────────────────────
    struct OffBehaviour<'a> {
        log: &'a Log,
    }

    impl<'a> Behavior<Event> for OffBehaviour<'a> {
        fn on_enter(&mut self, _current: &State) {
            self.log.push("enter_off");
        }

        fn on_exit(&mut self, _current: &State) {
            self.log.push("exit_off");
        }

        fn on_event(
            &mut self,
            event: &Event,
            _current: &State,
            tree: &dyn TreeLookup,
        ) -> EventReply {
            match event {
                Event::Toggle => {
                    let target = lookup!(tree, BlinkyState::On).unwrap();
                    EventReply::Transition(target)
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // On Behaviour
    // ─────────────────────────────────────────────────────────────
    struct OnBehaviour<'a> {
        log: &'a Log,
    }

    impl<'a> Behavior<Event> for OnBehaviour<'a> {
        fn on_enter(&mut self, _current: &State) {
            self.log.push("enter_on");
        }

        fn on_exit(&mut self, _current: &State) {
            self.log.push("exit_on");
        }

        fn on_event(
            &mut self,
            event: &Event,
            _current: &State,
            tree: &dyn TreeLookup,
        ) -> EventReply {
            match event {
                Event::Toggle => {
                    let target = lookup!(tree, BlinkyState::Off).unwrap();
                    EventReply::Transition(target)
                }
            }
        }
    }

    // -------------------------------------------------------------
    // Tests
    // -------------------------------------------------------------
    #[test]
    fn blinky_toggle() {
        // Build tree
        let mut tree = StateTree::new(BlinkyState::Root);

        let off = tree.add_child(&tree.root(), BlinkyState::Off);
        let on = tree.add_child(&tree.root(), BlinkyState::On);

        // lookup verification
        let off_state = lookup!(tree, BlinkyState::Off).unwrap();
        let on_state = lookup!(tree, BlinkyState::On).unwrap();

        assert_eq!(off, off_state);
        assert_eq!(on, on_state);

        // Behavior registration
        let log = Log::default();

        let mut behaviours = Behaviors::default();
        behaviours.register(&off_state, OffBehaviour { log: &log });
        behaviours.register(&on_state, OnBehaviour { log: &log });

        let runner = Runner::new(&tree);

        // Initial state: Off
        let mut current = off_state.clone();

        // -- First Toggle: Off -> On --
        current = runner.dispatch(&mut behaviours, &current, &Event::Toggle);

        assert_eq!(current, on_state);
        assert_eq!(log.take(), vec!["exit_off", "enter_on"]);

        // -- Second Toggle: On -> Off --
        current = runner.dispatch(&mut behaviours, &current, &Event::Toggle);

        assert_eq!(current, off_state);
        assert_eq!(log.take(), vec!["exit_on", "enter_off"]);
    }

    // -------------------------------------------------------------
    // Self-transition test (verify both exit + enter are triggered)
    // -------------------------------------------------------------
    #[test]
    fn self_transition() {
        let mut tree = StateTree::new(BlinkyState::Root);
        let _off = tree.add_child(&tree.root(), BlinkyState::Off);

        let off_state = lookup!(tree, BlinkyState::Off).unwrap();

        assert_eq!(off_state, _off);

        let log = Log::default();

        struct RealSelfBehaviour<'a> {
            log: &'a Log,
            me: State,
        }

        impl<'a> Behavior<Event> for RealSelfBehaviour<'a> {
            fn on_enter(&mut self, _current: &State) {
                self.log.push("enter");
            }

            fn on_exit(&mut self, _current: &State) {
                self.log.push("exit");
            }

            fn on_event(&mut self, _: &Event, _current: &State, _: &dyn TreeLookup) -> EventReply {
                EventReply::Transition(self.me.clone())
            }
        }

        let mut behaviours = Behaviors::default();
        behaviours.register(
            &off_state,
            RealSelfBehaviour {
                log: &log,
                me: off_state.clone(),
            },
        );

        let runner = Runner::new(&tree);

        let current = off_state.clone();
        let current = runner.dispatch(&mut behaviours, &current, &Event::Toggle);

        assert_eq!(*tree.kind(&current).unwrap(), BlinkyState::Off);

        assert_eq!(current, off_state);
        assert_eq!(log.take(), vec!["exit", "enter"]);
    }
}
