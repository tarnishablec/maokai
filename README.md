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
errors surface at runtime rather than compile time, and performance is strictly worse than a compile-time solution. If a
declarative library solves your problem, use that instead.

---

## Stateless runner

`Runner` holds only a reference to the tree. The caller owns the current state and behaviors.

One tree and one runner can drive any number of independent instances simultaneously — each with its own `State` and its
own `Behaviors`. No coordination required.

---

## Dispatch and transition are separate

`Runner::dispatch` bubbles an event through behaviors until one handles it. `Runner::transition` runs the exit/enter sequence between two states. The two are independent: routing an event does not change state, and changing state does not depend on any event.

Neither is `re-entrant`. Calling them from within `on_event`, `on_exit`, or `on_enter` is undefined behavior. This is a usage contract, not a type-level guarantee.

---

## Behavior declares intent

`Behavior<E>` returns an `EventReply`: handled (stop bubbling) or ignored (bubble to parent). It does not decide the next state. Deciding when and where to transition is the caller's responsibility — the runner only executes it.

A behavior can be tested without a runner and written without knowing the tree's shape.

---

&copy; 2026 [tarnishablec](https://github.com/tarnishablec) &mdash; Licensed under
the [Mozilla Public License 2.0](https://www.mozilla.org/en-US/MPL/2.0/)