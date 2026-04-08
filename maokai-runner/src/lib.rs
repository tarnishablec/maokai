#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
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

/// Describes a transition from one state to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: State,
    pub target: State,
    pub exit_list: Vec<State>,
    pub enter_list: Vec<State>,
}

/// Defines the life-cycle and event-handling logic for a specific state.
pub trait Behavior<E, Context = ()>: Send + Sync {
    /// Called when the state machine enters this state.
    fn on_enter(&self, _transition: &Transition, _context: &mut Context) {}
    /// Called when the state machine exits this state.
    fn on_exit(&self, _transition: &Transition, _context: &mut Context) {}
    /// Processes an incoming event.
    fn on_event(
        &self,
        event: &E,
        current: &State,
        context: &mut Context,
        tree: &dyn TreeView,
    ) -> EventReply;
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
        self.map.insert(state.clone(), Box::new(behavior));
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
            from: current.clone(),
            target: target.clone(),
            exit_list,
            enter_list,
        };

        // Notify states being exited (from leaf towards LCA)
        for state in &transition.exit_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_exit(&transition, context);
            }
        }

        // Notify states being entered (from LCA towards target leaf)
        for state in &transition.enter_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_enter(&transition, context);
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
    pub fn dispatch<E, Context>(
        &self,
        behaviors: &Behaviors<E, Context>,
        current: &State,
        event: &E,
        context: &mut Context,
    ) -> State {
        match self.bubble(behaviors, current, event, context) {
            EventReply::Transition(target) => self.transition(behaviors, current, &target, context),
            _ => current.clone(),
        }
    }

    /// Bubbles an event from the current state up to the root until handled or transitioned.
    fn bubble<E, Context>(
        &self,
        behaviors: &Behaviors<E, Context>,
        current: &State,
        event: &E,
        context: &mut Context,
    ) -> EventReply {
        let mut probe = current.clone();
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

    #[derive(Debug, Default, PartialEq, Eq)]
    struct Ctx {
        events: usize,
        exits: usize,
        enters: usize,
    }

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
        target: State,
    }

    struct CountingBehavior {
        target: State,
    }

    impl<'a> Behavior<Event> for BlinkyBehavior<'a> {
        fn on_enter(&self, transition: &Transition, _ctx: &mut ()) {
            self.log.push("enter", transition);
        }
        fn on_exit(&self, transition: &Transition, _ctx: &mut ()) {
            self.log.push("exit", transition);
        }
        fn on_event(&self, _e: &Event, _c: &State, _ctx: &mut (), _t: &dyn TreeView) -> EventReply {
            EventReply::Transition(self.target.clone())
        }
    }

    impl Behavior<Event, Ctx> for CountingBehavior {
        fn on_enter(&self, _transition: &Transition, ctx: &mut Ctx) {
            ctx.enters += 1;
        }

        fn on_exit(&self, _transition: &Transition, ctx: &mut Ctx) {
            ctx.exits += 1;
        }

        fn on_event(&self, _e: &Event, _c: &State, ctx: &mut Ctx, _t: &dyn TreeView) -> EventReply {
            ctx.events += 1;
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
        runner.dispatch(&behaviors, &off, &Event::Toggle, &mut ());
        let results = log.take();
        let expected = Transition {
            from: off.clone(),
            target: on.clone(),
            exit_list: alloc::vec![off.clone()],
            enter_list: alloc::vec![on.clone()],
        };
        let expected_enter = Transition { ..expected.clone() };

        // Verify that 'exit' received the 'off' handle and 'enter' received the 'on' handle
        assert_eq!(
            results,
            [("exit", expected.clone()), ("enter", expected_enter),]
        );

        // 2. Dispatch Toggle while in the 'On' state
        runner.dispatch(&behaviors, &on, &Event::Toggle, &mut ());
        let results = log.take();
        let expected = Transition {
            from: on.clone(),
            target: off.clone(),
            exit_list: alloc::vec![on.clone()],
            enter_list: alloc::vec![off.clone()],
        };
        let expected_enter = Transition { ..expected.clone() };

        // Verify reverse transition handles
        assert_eq!(
            results,
            [("exit", expected.clone()), ("enter", expected_enter),]
        );
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
        runner.dispatch(&behaviors, &off, &Event::Toggle, &mut ());

        let results = log.take();
        let expected = Transition {
            from: off.clone(),
            target: off.clone(),
            exit_list: alloc::vec![off.clone()],
            enter_list: alloc::vec![off.clone()],
        };
        // In a self-transition, both exit and enter should receive the same state handle
        assert_eq!(results, [("exit", expected.clone()), ("enter", expected),]);
    }

    #[test]
    fn custom_context_flows_through_event_and_transition() {
        let mut tree = StateTree::new(S::Root);
        let off = tree.add_child(&tree.root(), S::Off);
        let on = tree.add_child(&tree.root(), S::On);

        let mut behaviors: Behaviors<'_, Event, Ctx> = Behaviors::default();
        behaviors.register(&off, CountingBehavior { target: on.clone() });
        behaviors.register(
            &on,
            CountingBehavior {
                target: off.clone(),
            },
        );

        let runner = Runner::new(&tree);
        let mut ctx = Ctx::default();

        let next = runner.dispatch(&behaviors, &off, &Event::Toggle, &mut ctx);

        assert_eq!(next, on);
        assert_eq!(
            ctx,
            Ctx {
                events: 1,
                exits: 1,
                enters: 1,
            }
        );
    }
}
