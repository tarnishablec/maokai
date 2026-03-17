# maokai

A hierarchical state machine framework for Rust, built around a runtime-constructed state tree.

$$
\text{State}' = \text{Runner}\bigl(\text{Tree},\ \text{Behavior}[\text{State}],\ \text{Event}\bigr)
$$

The tree describes topology. Behaviors describe reactions. The runner is a pure function. None of the three owns the
others.

---

## Runtime tree

A compile-time state machine is a program. A runtime state tree is data.

Declarative state machine libraries — whether type-based like [statig](https://github.com/mdeloof/statig) or DSL-based
like [banish](https://github.com/LoganFlaherty/banish) — fix topology at compile time. When topology is data, it can be
loaded from a file, assembled from user input, or shared across a thousand instances without copying. The cost is that
errors surface at runtime rather than compile time. That tradeoff is the reason to choose maokai or not.

---

## Stateless runner

`Runner` holds only a reference to the tree. The caller owns the current state and behaviors.

One tree and one runner can drive any number of independent instances simultaneously — each with its own `State` and its
own `Behaviors`. No coordination required.

---

## RTC dispatch

Dispatch is event-driven and follows Run-To-Completion semantics. Each event is fully processed before the next can be
dispatched. The entire exit and enter sequence completes before `dispatch` returns.

Calling `dispatch` from within `on_event`, `on_exit`, or `on_enter` is undefined behavior. This is a usage contract, not
a type-level guarantee.

---

## Behavior declares intent

`Behavior<E>` returns an `EventReply`: do nothing, bubble to parent, or transition to a target state. It does not
execute the transition. The runner does.

A behavior can be tested without a runner and written without knowing the tree's shape.

---

&copy; 2026 [tarnishablec](https://github.com/tarnishablec) &mdash; Licensed under
the [Mozilla Public License 2.0](https://www.mozilla.org/en-US/MPL/2.0/)