# maokai-machine

`maokai-machine` is the instance layer of the Maokai state machine stack.

If `maokai-tree` defines topology, `maokai-runner` defines transition semantics, and `maokai-reconciler` defines an
operation pipeline, then `maokai-machine` is the crate that assembles those parts into a running state machine
instance.

It is intentionally small in scope. The machine is not the source of state topology, not the definition site for
behavior, and not the home of runtime-specific task logic. Its job is to hold the current state of one live instance
and to keep the system moving until it becomes stable.

---

## Role

`Machine` represents one active execution instance.

A machine owns:

- the current `State`
- the user-facing `Context`
- the reconciler and operation inbox for the instance
- the event queue that is ready to be dispatched
- the mapping from `Operation` type to `OpConsumer`
- the coordination loop that repeatedly commits operations, drains consumers, and dispatches resulting events

A machine does not own:

- the tree topology
- the behavior definitions
- the transition algorithm
- the task runtime itself

Those responsibilities stay in the other crates.

---

## Design Idea

The central design choice in this crate is that a machine is an orchestrator, not a framework-within-a-framework.

It does not try to understand every kind of operation. Instead, it provides a place where different operation
consumers can be registered and then driven in a uniform loop. This keeps the machine generic while still allowing
integration with event queues, task systems, or other side-effect channels.

The machine is therefore built around two ideas:

- state progression is explicit
- external effects re-enter the system through operations

This keeps execution deterministic at the machine boundary even when some consumers are backed by asynchronous runtimes.

---

## Relationship To The Other Crates

### `maokai-tree`

The tree owns structural truth. It decides which states exist, how they are related, and what path must be taken when
transitioning from one state to another.

### `maokai-runner`

The runner is the transition engine. It interprets behavior replies, performs bubbling, computes enter and exit
sequences, and updates state by applying the tree's transition path.

### `maokai-reconciler`

The reconciler is the staging area for operations. Behaviors and consumers can enqueue work there without executing it
immediately. The machine repeatedly commits that work until no more operations remain to be processed.

### `maokai-gears`

`maokai-gears` provides reusable operation consumers and runtime integrations, such as task consumers backed by Tokio.
Those integrations are attached to the machine as ordinary consumers. The machine does not treat them as built-in
features.

---

## What The Machine Actually Does

At runtime, the machine repeatedly performs a small coordination loop:

1. Commit staged operations from the reconciler.
2. Route each operation to its registered consumer.
3. Allow consumers to drain external inputs back into the reconciler.
4. Dispatch any ready events through the runner.
5. Repeat until no pending work remains.

This loop is the heart of the crate.

The point is not to hide the system from the user. The point is to give all moving parts one place to converge, so the
state machine can progress in a stable and predictable way.

---

## Event Handling

Events are built into the machine because they are part of its core execution model.

The machine owns a ready-event queue and installs an event consumer by default. This is the one operation path that is
treated as intrinsic rather than optional, because event dispatch is the primary mechanism by which the runner advances
state.

In contrast, task execution and other effect systems are not intrinsic. They enter through registered consumers.

---

## Consumers

Consumers are the machine's extension points.

Each consumer is responsible for one operation family. A consumer may:

- consume an operation immediately
- convert it into another operation
- drain externally-produced work back into the reconciler

This design lets the machine remain generic while still supporting rich integrations. A task system, for example, is
just another consumer family that understands task start and stop operations and later drains task-produced operations
back into the reconciler.

Because consumers are registered by operation type, the machine itself does not need to know the meaning of those
operations ahead of time.

---

## Task Integration

Task support in `maokai-machine` is intentionally indirect.

The machine still models task execution through consumers, but when a Tokio task feature is enabled it installs the
matching runtime-backed task consumer by default in `Machine::new`. That consumer is responsible for starting work,
stopping work, and draining task-produced operations back into the operation pipeline.

This separation is important:

- the machine remains runtime-agnostic
- task runtimes stay attached to task consumers instead of leaking into behavior code
- asynchronous task output becomes just another source of operations

As a result, local Tokio tasks, multi-threaded Tokio tasks, or custom runtimes can all fit the same machine model
without changing the machine's own design.

---

## Envelope

`Envelope<Event, Context>` is a machine snapshot and handle.

It is not the machine's storage object. The machine owns the real runtime state. The envelope is the cloneable view
that behaviors receive when the runner enters, exits, or dispatches against a state.

An envelope gives behaviors access to:

- the shared user-facing `Context`
- a `machine` handle that can post events and dispatch operations back into the instance

This separation is deliberate. The user-facing `Context` remains domain state, while machine runtime mechanics stay
inside `Machine`. The envelope is the bridge between the two.

---

## Why This Crate Exists

It would be possible to use the tree, runner, and reconciler directly. `maokai-machine` exists because real state
machines usually need one more layer: a concrete runtime instance that remembers where it is now and continues
processing until nothing else is pending.

That layer is small, but it matters.

Without it, the user has to manually coordinate:

- current state storage
- operation commit loops
- event queue draining
- consumer draining
- stabilization logic

`maokai-machine` gives those concerns a single home.

---

## What It Is Not

`maokai-machine` is not:

- a declarative statechart language
- a compile-time state machine generator
- a task runtime
- a behavior definition crate
- a full opinionated application framework

It is an execution shell around the rest of the Maokai model.

That narrow scope is deliberate. The crate is most useful when it stays focused on orchestration and leaves domain
semantics to behaviors and operation consumers.
