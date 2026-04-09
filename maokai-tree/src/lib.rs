#![no_std]

//! # maokai-tree
//!
//! `StateTree<T>` separates two concerns:
//!
//! - Topology: parent/child structure and transition path calculation.
//! - Semantics: payload `T` that describes what each state means.
//!
//! Runtime dispatch is handle-first: you typically cache `State` handles and transition with those handles.
//! The optional `lookup` feature provides convenience queries by payload, but it is not required for core runtime behavior.

extern crate alloc;

use alloc::vec::Vec;
use indextree::Arena;
use indextree::NodeId;

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Copy)]
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

    pub fn path_rev_iter(&self, state: &State) -> impl Iterator<Item = NodeId> {
        state.0.ancestors(&self.arena)
    }

    pub fn add_child(&mut self, parent: &State, data: T) -> State {
        let child = self.arena.new_node(data);
        parent.0.append(child, &mut self.arena);
        State(child)
    }
}

pub trait TreeView {
    fn contains(&self, state: &State) -> bool;
    fn parent_of(&self, state: &State) -> Option<State>;
    fn children_of(&self, state: &State) -> Vec<State>;

    fn root(&self) -> State;

    fn depth(&self, state: &State) -> usize;

    /// Calculates the transition path from current to target.
    ///
    /// Returns `(exit_list, enter_list)`.
    ///
    /// - `exit_list`: states to call `on_exit`, ordered `current -> LCA` (excluding LCA)
    /// - `enter_list`: states to call `on_enter`, ordered `LCA -> target` (excluding LCA)
    ///
    /// Self-transition (`current == target`) returns `([current], [target])`.
    fn propose_transition(&self, current: &State, target: &State) -> (Vec<State>, Vec<State>);
}

impl<T> TreeView for StateTree<T> {
    fn contains(&self, state: &State) -> bool {
        self.arena.get(state.0).is_some()
    }

    fn parent_of(&self, state: &State) -> Option<State> {
        state.0.ancestors(&self.arena).nth(1).map(State)
    }

    fn children_of(&self, state: &State) -> Vec<State> {
        state.0.children(&self.arena).map(State).collect()
    }
    fn root(&self) -> State {
        self.root
    }
    fn depth(&self, state: &State) -> usize {
        state.0.ancestors(&self.arena).count()
    }

    fn propose_transition(&self, current: &State, target: &State) -> (Vec<State>, Vec<State>) {
        if current == target {
            return (alloc::vec![*current], alloc::vec![*target]);
        }

        let current_path = self.path_rev_iter(current).collect::<Vec<_>>();
        let target_path = self.path_rev_iter(target).collect::<Vec<_>>();

        debug_assert!(
            current_path.last() == Some(&self.root.0) && target_path.last() == Some(&self.root.0),
            "State nodes must belong to the same StateTree root!",
        );

        let mut i = current_path.len();
        let mut j = target_path.len();

        while i > 0 && j > 0 && current_path[i - 1] == target_path[j - 1] {
            i -= 1;
            j -= 1;
        }

        let exit_list: Vec<State> = current_path[..i].iter().copied().map(State).collect();

        let mut enter_list: Vec<State> = target_path[..j].iter().copied().map(State).collect();
        enter_list.reverse();

        (exit_list, enter_list)
    }
}

pub trait DataView<T>: TreeView {
    fn get_data(&self, state: &State) -> Option<&T>;

    fn travel<'a>(&'a self, state: &'a State) -> impl Iterator<Item = &'a T> + 'a
    where
        T: 'a;
}

impl<T> DataView<T> for StateTree<T> {
    fn get_data(&self, state: &State) -> Option<&T> {
        self.arena.get(state.0).map(|node| node.get())
    }

    /// Iterates payload data from root to `state`.
    fn travel<'a>(&'a self, state: &'a State) -> impl Iterator<Item = &'a T> + 'a
    where
        T: 'a,
    {
        let mut ids: Vec<NodeId> = self.path_rev_iter(state).collect();
        ids.reverse();
        ids.into_iter()
            .filter_map(move |id| self.arena.get(id).map(|n| n.get()))
    }
}

pub trait Lookup<T> {
    fn lookup(&self, dataset: &[&T]) -> Option<State>;
}

#[cfg(feature = "lookup")]
impl<T, U> Lookup<T> for U
where
    T: PartialEq,
    U: DataView<T>,
{
    fn lookup(&self, dataset: &[&T]) -> Option<State> {
        debug_assert!(dataset.len() <= 64, "dataset length must be <= 64");

        fn dfs<T: PartialEq, V: DataView<T> + ?Sized>(
            view: &V,
            current: State,
            dataset: &[&T],
            mut unmatched: u64,
        ) -> Option<State> {
            if let Some(node_data) = view.get_data(&current) {
                for (i, &target_data) in dataset.iter().enumerate() {
                    if unmatched & (1 << i) != 0 && target_data == node_data {
                        unmatched ^= 1 << i;
                        break;
                    }
                }
            }

            if unmatched == 0 {
                return Some(current);
            }

            for child in view.children_of(&current) {
                if let Some(found) = dfs(view, child, dataset, unmatched) {
                    return Some(found);
                }
            }
            None
        }

        let unmatched = (1u64 << dataset.len()) - 1;
        dfs(self, self.root(), dataset, unmatched)
    }
}

#[cfg(feature = "lookup")]
#[macro_export]
macro_rules! lookup {
    ($tree:expr, $($data:expr),+) => {
        $crate::Lookup::lookup(&$tree, &[$(&$data),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum S {
        Root,
        A,
        B,
        C,
        D,
    }

    fn build_tree() -> (StateTree<S>, State, State, State, State) {
        let mut tree = StateTree::new(S::Root);
        let a = tree.add_child(&tree.root(), S::A);
        let b = tree.add_child(&a, S::B);
        let c = tree.add_child(&a, S::C);
        let d = tree.add_child(&tree.root(), S::D);
        (tree, a, b, c, d)
    }

    #[test]
    fn parent_lookup_works() {
        let (tree, a, b, _, d) = build_tree();
        assert_eq!(tree.parent_of(&a), Some(tree.root()));
        assert_eq!(tree.parent_of(&b), Some(a));
        assert_eq!(tree.parent_of(&d), Some(tree.root()));
        assert_eq!(tree.parent_of(&tree.root()), None);
    }

    #[test]
    fn children_lookup_works() {
        let (tree, a, b, c, d) = build_tree();
        assert_eq!(tree.children_of(&a), alloc::vec![b, c]);
        assert_eq!(tree.children_of(&tree.root()), alloc::vec![a, d]);
    }

    #[test]
    fn contains_checks_existing_state() {
        let (tree, a, b, c, d) = build_tree();
        assert!(tree.contains(&tree.root()));
        assert!(tree.contains(&a));
        assert!(tree.contains(&b));
        assert!(tree.contains(&c));
        assert!(tree.contains(&d));
    }

    #[test]
    fn transition_between_siblings() {
        let (tree, _, b, c, _) = build_tree();
        let (exit, enter) = tree.propose_transition(&b, &c);
        assert_eq!(exit, alloc::vec![b]);
        assert_eq!(enter, alloc::vec![c]);
    }

    #[test]
    fn transition_across_branches() {
        let (tree, a, b, _, d) = build_tree();
        let (exit, enter) = tree.propose_transition(&b, &d);
        assert_eq!(exit, alloc::vec![b, a]);
        assert_eq!(enter, alloc::vec![d]);
    }

    #[test]
    fn self_transition_emits_exit_and_enter() {
        let (tree, _, b, _, _) = build_tree();
        let (exit, enter) = tree.propose_transition(&b, &b);
        assert_eq!(exit, alloc::vec![b]);
        assert_eq!(enter, alloc::vec![b]);
    }

    #[test]
    fn travel_is_root_to_leaf() {
        let (tree, _, b, _, _) = build_tree();
        let data = tree.travel(&b).cloned().collect::<Vec<_>>();
        assert_eq!(data, alloc::vec![S::Root, S::A, S::B]);
    }
}

#[cfg(all(test, feature = "lookup"))]
mod lookup_tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum S {
        Root,
        A,
        B,
        C,
        D,
    }

    fn build_tree() -> (StateTree<S>, State, State, State, State) {
        let mut tree = StateTree::new(S::Root);
        let a = tree.add_child(&tree.root(), S::A);
        let b = tree.add_child(&a, S::B);
        let c = tree.add_child(&a, S::C);
        let d = tree.add_child(&tree.root(), S::D);
        (tree, a, b, c, d)
    }

    #[test]
    fn match_root() {
        let (tree, _, _, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::Root), Some(tree.root()));
    }

    #[test]
    fn match_non_leaf() {
        let (tree, a, _, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::A), Some(a));
    }

    #[test]
    fn match_leaf() {
        let (tree, _, b, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::B), Some(b));
    }

    #[test]
    fn match_deepest_ordered() {
        let (tree, _, b, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::A, S::B), Some(b));
    }

    #[test]
    fn match_deepest_unordered() {
        let (tree, _, b, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::B, S::A), Some(b));
    }

    #[test]
    fn match_sibling() {
        let (tree, _, _, c, _) = build_tree();
        assert_eq!(lookup!(tree, S::A, S::C), Some(c));
    }

    #[test]
    fn match_root_to_leaf() {
        let (tree, _, b, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::Root, S::A, S::B), Some(b));
    }

    #[test]
    fn no_match_across_branches() {
        let (tree, _, _, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::B, S::D), None);
    }

    #[test]
    fn no_match_sibling_leaves() {
        let (tree, _, _, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::B, S::C), None);
    }

    #[test]
    fn no_match_unknown() {
        let (tree, _, _, _, _) = build_tree();
        assert_eq!(lookup!(tree, S::D, S::B), None);
    }
}
