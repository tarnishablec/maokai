#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use maokai_tree::{State, StateTree, TreeView};

pub enum EventReply {
    /// Event handled, no transition.
    Handled,
    /// Event ignored, bubbles to parent state.
    Ignored,
    /// Trigger transition (target == current is allowed and treated as self-transition).
    Transition(State),
}

pub trait Behavior<E>: Send + Sync {
    fn on_enter(&self, _current: &State) {}
    fn on_exit(&self, _current: &State) {}
    fn on_event(&self, event: &E, current: &State, tree: &dyn TreeView) -> EventReply;
}

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
    pub fn register(&mut self, state: &State, behavior: impl Behavior<E> + 'a) {
        self.map.insert(state.clone(), Box::new(behavior));
    }
}

pub struct Runner<'a, T> {
    tree: &'a StateTree<T>,
}

impl<'a, T> Runner<'a, T> {
    pub fn new(tree: &'a StateTree<T>) -> Self {
        Self { tree }
    }

    /// Executes transition sequence: all exits first, then all enters.
    pub fn transition<E>(
        &self,
        behaviors: &Behaviors<E>,
        current: &State,
        target: &State,
    ) -> State {
        let (exit_list, enter_list) = self.tree.propose_transition(current, target);

        for state in &exit_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_exit(current);
            }
        }

        for state in &enter_list {
            if let Some(behavior) = behaviors.map.get(state) {
                behavior.on_enter(current);
            }
        }

        target.clone()
    }
}

impl<'a, T> Runner<'a, T>
where
    StateTree<T>: TreeView,
{
    /// Dispatches one event with RTC semantics.
    pub fn dispatch<E>(&self, behaviors: &Behaviors<E>, current: &State, event: &E) -> State {
        match self.bubble(behaviors, current, event) {
            EventReply::Transition(target) => {
                self.transition(behaviors, current, &target);
                target
            }
            EventReply::Handled | EventReply::Ignored => current.clone(),
        }
    }

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
                None => return EventReply::Handled,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    extern crate alloc;
    extern crate std;

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum BlinkyState {
        Root,
        Off,
        On,
    }

    #[derive(Debug)]
    enum Event {
        Toggle,
    }

    #[derive(Default)]
    struct Log {
        entries: Mutex<Vec<&'static str>>,
    }

    impl Log {
        fn push(&self, entry: &'static str) {
            self.entries.lock().unwrap().push(entry);
        }

        fn take(&self) -> Vec<&'static str> {
            core::mem::take(&mut *self.entries.lock().unwrap())
        }
    }

    struct OffBehavior<'a> {
        log: &'a Log,
        target_on: State,
    }

    impl<'a> Behavior<Event> for OffBehavior<'a> {
        fn on_enter(&self, _current: &State) {
            self.log.push("enter_off");
        }

        fn on_exit(&self, _current: &State) {
            self.log.push("exit_off");
        }

        fn on_event(&self, event: &Event, _current: &State, _tree: &dyn TreeView) -> EventReply {
            match event {
                Event::Toggle => EventReply::Transition(self.target_on.clone()),
            }
        }
    }

    struct OnBehavior<'a> {
        log: &'a Log,
        target_off: State,
    }

    impl<'a> Behavior<Event> for OnBehavior<'a> {
        fn on_enter(&self, _current: &State) {
            self.log.push("enter_on");
        }

        fn on_exit(&self, _current: &State) {
            self.log.push("exit_on");
        }

        fn on_event(&self, event: &Event, _current: &State, _tree: &dyn TreeView) -> EventReply {
            match event {
                Event::Toggle => EventReply::Transition(self.target_off.clone()),
            }
        }
    }

    #[test]
    fn blinky_toggle() {
        let mut tree = StateTree::new(BlinkyState::Root);
        let off = tree.add_child(&tree.root(), BlinkyState::Off);
        let on = tree.add_child(&tree.root(), BlinkyState::On);

        let log = Log::default();
        let mut behaviors = Behaviors::default();
        behaviors.register(
            &off,
            OffBehavior {
                log: &log,
                target_on: on.clone(),
            },
        );
        behaviors.register(
            &on,
            OnBehavior {
                log: &log,
                target_off: off.clone(),
            },
        );

        let runner = Runner::new(&tree);
        let mut current = off.clone();

        current = runner.dispatch(&behaviors, &current, &Event::Toggle);
        assert_eq!(current, on);
        assert_eq!(log.take(), vec!["exit_off", "enter_on"]);

        current = runner.dispatch(&behaviors, &current, &Event::Toggle);
        assert_eq!(current, off);
        assert_eq!(log.take(), vec!["exit_on", "enter_off"]);
    }

    #[test]
    fn self_transition_triggers_exit_then_enter() {
        let mut tree = StateTree::new(BlinkyState::Root);
        let off = tree.add_child(&tree.root(), BlinkyState::Off);

        let log = Log::default();

        struct SelfBehavior<'a> {
            log: &'a Log,
            me: State,
        }

        impl<'a> Behavior<Event> for SelfBehavior<'a> {
            fn on_enter(&self, _current: &State) {
                self.log.push("enter");
            }

            fn on_exit(&self, _current: &State) {
                self.log.push("exit");
            }

            fn on_event(
                &self,
                _event: &Event,
                _current: &State,
                _tree: &dyn TreeView,
            ) -> EventReply {
                EventReply::Transition(self.me.clone())
            }
        }

        let mut behaviors = Behaviors::default();
        behaviors.register(
            &off,
            SelfBehavior {
                log: &log,
                me: off.clone(),
            },
        );

        let runner = Runner::new(&tree);
        let current = runner.dispatch(&behaviors, &off, &Event::Toggle);

        assert_eq!(current, off);
        assert_eq!(tree.kind(&current), Some(&BlinkyState::Off));
        assert_eq!(log.take(), vec!["exit", "enter"]);
    }
}