#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::any::Any;
use indextree::Arena;
use indextree::NodeId;

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct State(pub(crate) NodeId);

#[derive(Debug, Clone)]
pub struct StateTree<T> {
    arena: Arena<T>,
    root: State,
}

impl<T> StateTree<T> {
    pub fn new(root_data: T) -> Self {
        let mut arena = Arena::new();
        let root = arena.new_node(root_data);
        Self {
            arena,
            root: State(root),
        }
    }

    pub fn add_child(&mut self, parent: &State, data: T) -> State {
        let child = self.arena.new_node(data);
        parent.0.append(child, &mut self.arena);
        State(child)
    }

    pub fn root(&self) -> State {
        self.root.clone()
    }

    /// Calculates the transition path from current to target.
    ///
    /// Returns (exit_list, enter_list):
    /// - `exit_list`: list of states that need on_exit, in order current -> LCA (excluding LCA)
    /// - `enter_list`: list of states that need on_enter, in order LCA -> target (excluding LCA)
    ///
    /// Self-transition (current == target): returns ([current], [target]),
    /// ensuring both on_exit + on_enter are triggered.
    pub fn propose_transition(&self, current: &State, target: &State) -> (Vec<State>, Vec<State>) {
        // Special handling for self-transition: exit then enter self
        if current == target {
            return (alloc::vec![current.clone()], alloc::vec![target.clone()]);
        }

        let current_path = self.path_rev(current);
        let target_path = self.path_rev(target);

        debug_assert!(
            current_path.last() == Some(&self.root.0) && target_path.last() == Some(&self.root.0),
            "State nodes must belong to the same StateTree root!",
        );

        let mut i = current_path.len();
        let mut j = target_path.len();

        // Contract from root side to find LCA
        while i > 0 && j > 0 && current_path[i - 1] == target_path[j - 1] {
            i -= 1;
            j -= 1;
        }

        // exit_list: current to LCA (excluding LCA), in current -> LCA direction
        let exit_list: Vec<State> = current_path[..i].iter().copied().map(State).collect();

        // enter_list: LCA to target (excluding LCA), in LCA -> target direction
        let mut enter_list: Vec<State> = target_path[..j].iter().copied().map(State).collect();
        enter_list.reverse();

        (exit_list, enter_list)
    }

    /// Returns [state, ..., root] path (including state itself)
    pub fn path_rev(&self, state: &State) -> Vec<NodeId> {
        state.0.ancestors(&self.arena).collect::<Vec<_>>()
    }

    /// Data iterator from root -> state
    pub fn travel<'a>(&'a self, state: &'a State) -> impl Iterator<Item = &'a T> {
        self.path_rev(state)
            .into_iter()
            .rev()
            .filter_map(move |id| self.arena.get(id).map(|n| n.get()))
    }

    pub fn contains(&self, state: &State) -> bool {
        self.arena.get(state.0).is_some()
    }

    pub fn parent_of(&self, state: &State) -> Option<State> {
        state.0.ancestors(&self.arena).nth(1).map(State)
    }

    pub fn children_of(&self, state: &State) -> Vec<State> {
        state.0.children(&self.arena).map(State).collect()
    }
}

impl<T: Any + PartialEq> StateTree<T> {
    pub fn find(&self, data: &dyn Any) -> Option<State> {
        self.arena
            .iter()
            .find(|node| match data.downcast_ref::<T>() {
                Some(target) => node.get() == target,
                None => false,
            })
            .map(|node| self.arena.get_node_id(node))
            .flatten()
            .map(State)
    }
}
