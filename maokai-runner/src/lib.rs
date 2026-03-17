#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use maokai_tree::{State, StateTree};

// ── 公开类型 ──────────────────────────────────────────────────────────────────

pub enum EventReply {
    /// 事件已处理，不转移
    Handled,
    /// 事件未处理，向父状态冒泡
    Ignored,
    /// 触发转移（允许 target == current，视为自转移，会触发 exit + enter）
    Transition(State),
}

pub trait Behaviour<E> {
    fn on_enter(&mut self) {}
    fn on_exit(&mut self) {}
    fn on_event(&mut self, event: &E) -> EventReply;
}

// ── Behaviours：外置的行为注册表（调用方持有） ────────────────────────────────

pub struct Behaviours<E> {
    map: BTreeMap<State, Box<dyn Behaviour<E>>>,
}

impl<E> Behaviours<E> {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, state: &State, behaviour: impl Behaviour<E> + 'static) {
        self.map.insert(state.clone(), Box::new(behaviour));
    }
}

// ── Runner：无状态计算器 ──────────────────────────────────────────────────────

pub struct Runner<'a, T> {
    tree: &'a StateTree<T>,
}

impl<'a, T> Runner<'a, T> {
    pub fn new(tree: &'a StateTree<T>) -> Self {
        Self { tree }
    }

    /// 投递事件，RTC 语义：
    /// - 从 current 开始向祖先冒泡，找到第一个处理事件的状态
    /// - 若返回 Transition，执行转移序列（exit / enter）
    /// - 返回转移后的最终状态
    ///
    /// 注意：no re-entrant。on_event / on_exit / on_enter 中不应再调用 dispatch。
    pub fn dispatch<E>(&self, behaviours: &mut Behaviours<E>, current: &State, event: &E) -> State {
        match self.bubble(behaviours, current, event) {
            EventReply::Transition(target) => {
                self.transition(behaviours, current, &target);
                target
            }
            EventReply::Handled | EventReply::Ignored => current.clone(),
        }
    }

    /// 执行转移序列：exit_list 依次 on_exit，enter_list 依次 on_enter
    pub fn transition<E>(
        &self,
        behaviours: &mut Behaviours<E>,
        current: &State,
        target: &State,
    ) -> State {
        let (exit_list, enter_list) = self.tree.propose_transition(current, target);

        for state in &exit_list {
            if let Some(b) = behaviours.map.get_mut(state) {
                b.on_exit();
            }
        }

        for state in &enter_list {
            if let Some(b) = behaviours.map.get_mut(state) {
                b.on_enter();
            }
        }

        target.clone()
    }

    // ── 私有实现 ──────────────────────────────────────────────────────────────

    /// 向祖先冒泡，跳过没有注册 behaviour 的节点，直到 root
    fn bubble<E>(&self, behaviours: &mut Behaviours<E>, current: &State, event: &E) -> EventReply {
        let mut probe = current.clone();

        loop {
            if let Some(b) = behaviours.map.get_mut(&probe) {
                match b.on_event(event) {
                    EventReply::Ignored => {}
                    other => return other, // Handled 或 Transition
                }
            }
            match self.tree.parent_of(&probe) {
                Some(parent) => probe = parent,
                None => return EventReply::Handled, // 到 root 没人处理，视为吞掉
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    // ── 测试基础设施 ──────────────────────────────────────────────────────────

    #[derive(Debug, PartialEq)]
    enum Event {
        A,
        Reset,
    }

    type Log = Rc<RefCell<Vec<&'static str>>>;

    struct TrackedBehaviour {
        name: &'static str,
        log: Log,
        reply: Box<dyn Fn(&Event) -> EventReply>,
    }

    impl TrackedBehaviour {
        fn new(
            name: &'static str,
            log: Log,
            reply: impl Fn(&Event) -> EventReply + 'static,
        ) -> Self {
            Self {
                name,
                log,
                reply: Box::new(reply),
            }
        }
    }

    impl Behaviour<Event> for TrackedBehaviour {
        fn on_enter(&mut self) {
            self.log
                .borrow_mut()
                .extend_from_slice(&[self.name, "enter"]);
        }
        fn on_exit(&mut self) {
            self.log
                .borrow_mut()
                .extend_from_slice(&[self.name, "exit"]);
        }
        fn on_event(&mut self, event: &Event) -> EventReply {
            (self.reply)(event)
        }
    }

    // ── 测试状态树 ────────────────────────────────────────────────────────────
    //
    //   root
    //   └─ parent
    //      ├─ child_a
    //      └─ child_b

    struct TestFixture {
        tree: StateTree<&'static str>,
        #[allow(dead_code)]
        root: State,
        parent: State,
        child_a: State,
        child_b: State,
        log: Log,
    }

    impl TestFixture {
        fn new() -> Self {
            let mut tree = StateTree::new("root");
            let root = tree.root().clone();
            let parent = tree.add_child(&root, "parent");
            let child_a = tree.add_child(&parent, "child_a");
            let child_b = tree.add_child(&parent, "child_b");
            Self {
                tree,
                root,
                parent,
                child_a,
                child_b,
                log: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn runner(&'_ self) -> Runner<'_, &'static str> {
            Runner::new(&self.tree)
        }

        /// 注册一个带日志追踪的 behaviour
        fn tracked(
            &self,
            name: &'static str,
            reply: impl Fn(&Event) -> EventReply + 'static,
        ) -> TrackedBehaviour {
            TrackedBehaviour::new(name, self.log.clone(), reply)
        }

        fn log(&self) -> Vec<&'static str> {
            self.log.borrow().clone()
        }
    }

    // ── 测试用例 ──────────────────────────────────────────────────────────────

    /// Handled：状态不变，不触发 exit/enter
    #[test]
    fn test_handled_no_transition() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        b.register(&f.child_a, f.tracked("child_a", |_| EventReply::Handled));

        assert_eq!(runner.dispatch(&mut b, &f.child_a, &Event::A), f.child_a);
        assert!(f.log().is_empty());
    }

    /// 兄弟转移：child_a -> child_b，parent 是 LCA 不触发
    #[test]
    fn test_transition_sibling() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        let child_b = f.child_b.clone();
        b.register(
            &f.child_a,
            f.tracked("child_a", move |_| EventReply::Transition(child_b.clone())),
        );
        b.register(&f.parent, f.tracked("parent", |_| EventReply::Handled));
        b.register(&f.child_b, f.tracked("child_b", |_| EventReply::Handled));

        assert_eq!(runner.dispatch(&mut b, &f.child_a, &Event::A), f.child_b);
        assert_eq!(f.log(), ["child_a", "exit", "child_b", "enter"]);
    }

    /// 冒泡：child_a Ignored，parent 处理并转移
    #[test]
    fn test_bubble_to_parent() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        let child_b = f.child_b.clone();
        b.register(&f.child_a, f.tracked("child_a", |_| EventReply::Ignored));
        b.register(
            &f.parent,
            f.tracked("parent", move |_| EventReply::Transition(child_b.clone())),
        );

        assert_eq!(runner.dispatch(&mut b, &f.child_a, &Event::A), f.child_b);
    }

    /// 冒泡穿越空洞：中间节点没有注册 behaviour，继续向上
    #[test]
    fn test_bubble_through_hole() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        // parent 和 root 都没注册，child_a Ignored 后事件被吞掉
        b.register(&f.child_a, f.tracked("child_a", |_| EventReply::Ignored));

        assert_eq!(runner.dispatch(&mut b, &f.child_a, &Event::A), f.child_a);
    }

    /// 自转移：Transition(current) 触发 exit + enter
    #[test]
    fn test_self_transition() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        let child_a = f.child_a.clone();
        b.register(
            &f.child_a,
            f.tracked("child_a", move |e| match e {
                Event::Reset => EventReply::Transition(child_a.clone()),
                _ => EventReply::Handled,
            }),
        );

        assert_eq!(
            runner.dispatch(&mut b, &f.child_a, &Event::Reset),
            f.child_a
        );
        assert_eq!(f.log(), ["child_a", "exit", "child_a", "enter"]);
    }

    /// 跨层转移：child_a -> child_b，确认 parent（LCA）不出现在日志中
    #[test]
    fn test_lca_not_triggered() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        let child_b = f.child_b.clone();
        b.register(
            &f.child_a,
            f.tracked("child_a", move |_| EventReply::Transition(child_b.clone())),
        );
        b.register(&f.parent, f.tracked("parent", |_| EventReply::Handled));
        b.register(&f.child_b, f.tracked("child_b", |_| EventReply::Handled));

        runner.dispatch(&mut b, &f.child_a, &Event::A);
        // parent 是 LCA，不应出现
        assert!(!f.log().contains(&"parent"));
        assert_eq!(f.log(), ["child_a", "exit", "child_b", "enter"]);
    }

    /// 手动 transition
    #[test]
    fn test_manual_transition() {
        let f = TestFixture::new();
        let runner = f.runner();
        let mut b = Behaviours::new();
        b.register(&f.child_a, f.tracked("child_a", |_| EventReply::Handled));
        b.register(&f.child_b, f.tracked("child_b", |_| EventReply::Handled));

        assert_eq!(runner.transition(&mut b, &f.child_a, &f.child_b), f.child_b);
        assert_eq!(f.log(), ["child_a", "exit", "child_b", "enter"]);
    }
}
