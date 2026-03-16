#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use indextree::Arena;
pub use indextree::NodeId;

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct State(pub(crate) Vec<NodeId>);

impl State {
    pub fn depth(&self) -> usize {
        self.0.len()
    }

    pub fn leaf(&self) -> NodeId {
        self.0
            .last()
            .cloned()
            .expect("State must have at least one node")
    }

    pub fn to_path(&self) -> Vec<NodeId> {
        self.0.clone()
    }
}

#[derive(Debug, Clone)]
pub struct StateTree<T> {
    arena: Arena<T>,
    root: NodeId,
}

impl<T> StateTree<T> {
    /// 创建一棵树，必须指定一个根
    pub fn new(root_data: T) -> Self {
        let mut arena = Arena::new();
        let root = arena.new_node(root_data);
        Self { arena, root }
    }

    /// 插入子节点：业务层通过此接口扩展拓扑
    pub fn add_child(&mut self, parent: NodeId, data: T) -> NodeId {
        let child = self.arena.new_node(data);
        parent.append(child, &mut self.arena);
        child
    }

    /// 获取根节点 ID
    pub fn root(&self) -> NodeId {
        self.root
    }
    pub fn propose_transition(
        &self,
        current: &State,
        target: &State,
    ) -> (Vec<NodeId>, Vec<NodeId>) {
        // 1. 溯源目标路径：[root, ..., target]
        let mut target_path: Vec<NodeId> = target.to_path();
        target_path.reverse();

        // 2. 寻找 LCA (最小公共祖先)
        let mut common_count = 0;
        for (n1, n2) in current.0.iter().zip(target_path.iter()) {
            if n1 == n2 {
                common_count += 1;
            } else {
                break;
            }
        }

        // Exit：从当前路径的末尾向上到 LCA 之前
        let exit_list = current.0[common_count..].iter().rev().cloned().collect();

        // Enter：从目标路径的 LCA 之后向下到 target
        let enter_list = target_path[common_count..].iter().cloned().collect();

        (exit_list, enter_list)
    }

    pub fn path_to(&self, node: &NodeId) -> Vec<NodeId> {
        node.ancestors(&self.arena)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn state_of(&self, node: &NodeId) -> State {
        State(self.path_to(node))
    }

    /// 向下巡游 (Downwards Travel)：从 Root 到 Leaf。
    pub fn travel<'a>(&'a self, state: &'a State) -> impl DoubleEndedIterator<Item = &'a T> {
        state
            .0
            .iter()
            .filter_map(|&id| self.arena.get(id))
            .map(|n| n.get())
    }

    pub fn get_node(&self, id: NodeId) -> Option<&T> {
        self.arena.get(id).map(|n| n.get())
    }
}
