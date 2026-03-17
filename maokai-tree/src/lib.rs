#![no_std]

extern crate alloc;

use alloc::vec::Vec;
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

    pub fn root(&self) -> &State {
        &self.root
    }

    /// 计算从 current 到 target 的转移路径。
    ///
    /// 返回 (exit_list, enter_list)：
    /// - exit_list: 需要 on_exit 的状态列表，顺序为 current -> LCA（不含 LCA）
    /// - enter_list: 需要 on_enter 的状态列表，顺序为 LCA -> target（不含 LCA）
    ///
    /// 自转移（current == target）：返回 ([current], [target])，
    /// 确保 on_exit + on_enter 都被触发。
    pub fn propose_transition(&self, current: &State, target: &State) -> (Vec<State>, Vec<State>) {
        // 自转移特殊处理：exit 再 enter 自身
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

        // 从 root 端向内收缩，找到 LCA
        while i > 0 && j > 0 && current_path[i - 1] == target_path[j - 1] {
            i -= 1;
            j -= 1;
        }

        // exit_list: current 到 LCA（不含 LCA），顺序 current -> LCA 方向
        let exit_list: Vec<State> = current_path[..i].iter().copied().map(State).collect();

        // enter_list: LCA 到 target（不含 LCA），顺序 LCA -> target 方向
        let mut enter_list: Vec<State> = target_path[..j].iter().copied().map(State).collect();
        enter_list.reverse();

        (exit_list, enter_list)
    }

    /// 返回 [state, ..., root] 路径（包含 state 自身）
    pub fn path_rev(&self, state: &State) -> Vec<NodeId> {
        state.0.ancestors(&self.arena).collect::<Vec<_>>()
    }

    /// root -> state 数据迭代器
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
        // ancestors() 第 0 个是自身，第 1 个是 parent
        state.0.ancestors(&self.arena).nth(1).map(State)
    }

    pub fn children_of(&self, state: &State) -> Vec<State> {
        state.0.children(&self.arena).map(State).collect()
    }
}
