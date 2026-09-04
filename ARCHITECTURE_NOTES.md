# Architecture Notes

> Design discussion captured 2026-09-04. Covers the top-level shape of the engine:
> the two-world split, what owns which OS thread, how the two worlds talk to each
> other, and the naming decisions behind it. Chunk-loading-specific material lives in
> `CHUNK_LOADING_NOTES.md` and just points back here for the general mechanics.

## 0. The two worlds

The engine runs **two ECS worlds on two threads**:

- **Primary world** — windowing, rendering, UI, input. Always exists, even with no
  game loaded (e.g. main menu) — it's the process's actual entry point.
- **Sim world (20 Hz)** — terrain gen, the edit overlay, entity simulation, the chunk
  production pipeline, and Replicon (wears a client or server hat depending on
  connection). Only exists while a game session is active, SP or MP.

Splitting them onto separate threads means Sim's heavy 20 Hz work runs *concurrently*
with Primary's frame loop instead of stalling it — see `CHUNK_LOADING_NOTES.md` §0 for
the chunk-work motivation that originally drove this split. It also matches the
Replicon MP commitment so SP and MP don't fork.

---

## 1. Naming

**Primary**, not Gameplay/Main/Client/Shell/Frontend. Landed here after several
rejected options — worth keeping the reasoning so it isn't re-litigated:

- **Not "Gameplay"** — the world also owns windowing, rendering, UI, and is sometimes
  the *only* world running (main menu, no game loaded), so "gameplay" undersold it.
- **Not "Client"** (for either world's id/prefix) — the *Sim* world can itself wear a
  Replicon client hat depending on connection (§0), so "Client" already has a
  specific, contradictory meaning in this system. This was the sharpest rejection:
  the same word pointing at *opposite* things in the same sentence.
- **Not "Main"** — collides with real Bevy vocabulary in the exact dependency
  version pinned: `bevy_app::Main` is the schedule label every `App` (including Sim)
  runs each `update()`, and `App::main()`/`main_mut()` is the accessor for Bevy's own
  internal primary `SubApp`. Not fatal, but real ambiguity since Sim also has a `Main`
  schedule.
- **Not "Shell" / "Frontend"** — rejected on taste, not correctness.
- **Not "Core" / "Base"** — `Core` already names the render bootstrap module
  (`render/core.rs`, `CoreRenderPlugin`); "base terrain" is already a load-bearing
  term in `CHUNK_LOADING_NOTES.md` §1.
- **"Primary" holds up** — "the primary window" / "the primary monitor" (real winit
  and Bevy concepts) use "primary" the same way this world's name does: the main one
  among possibly-several. That's a *hierarchical* relationship (the Primary world
  owns the primary window), not a contradiction — unlike the Client case above.

**The actual test for a name collision going forward:** does the word already mean
something *different or opposite* elsewhere in this system (real problem), not
whether the word appears anywhere in the dependency tree (usually fine).

---

## 2. Primary world: ownership & threading

Primary's windowing/wgpu foundation comes from `modul_core`/`modul_render` (own
project, `EliiasG/modul`). This shapes how Primary is built, not just what it draws:

- **`modul_core::run_app(initializer, setup)` is the actual process entry point.** It
  owns the OS thread and the winit event loop itself, and never returns — `main()`
  just calls it. It hands the `setup` closure `&mut SubApp`, not `&mut App`.
- **Primary is therefore a `SubApp`, not a full `App`.** This is not a limitation for
  ordinary Bevy plugins: `SubApp::add_plugins` internally wraps itself in a temporary
  `App` to satisfy `Plugin::build(&mut App)`, then unwraps — confirmed in
  `bevy_app`'s source. `sub_app.add_plugins(AnyBevyPlugin)` works exactly like it
  would on a real `App`.
- **Exit is bespoke, not `AppExit`.** modul has no full `App`, so it uses its own
  `ShouldExit` marker resource (inserted by `modul_util::ExitPlugin` on
  window-close, checked by modul's winit handler after each `SubApp::update()`)
  instead of Bevy's standard `AppExit`/`should_exit()`. Anything that needs to react
  to Primary shutting down (e.g. telling Sim to stop) watches for `ShouldExit`.
- **Schedules:** `PreInit` (before window/GPU exist), `Init` (right after GPU/window
  setup), `Redraw` (the per-frame schedule, driven by winit's `RedrawRequested` —
  i.e. display-paced, not free-spinning).
- **Papercut:** a few of modul's ergonomic extension traits (e.g.
  `modul_asset::AssetAppExt::init_assets`) are `impl ... for App`, not `SubApp`, so
  they only work from inside a `Plugin::build` (which gets the temporary real `App`)
  — not directly in the top-level `setup` closure. Use `world_mut().insert_resource`
  directly there instead.

---

## 3. Sim world: ownership & threading

Sim has no windowing/GPU state at all, so unlike Primary it doesn't need to live on
any particular thread for platform reasons — it's a genuine standalone `App`, built
and moved wholesale into its own `std::thread::spawn` closure:

```rust
let sim_handle = std::thread::Builder::new()
    .name("sim-world".into())
    .spawn(move || {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(
            Duration::from_secs_f64(1.0 / 20.0),
        )))
        .add_plugins((RepliconPlugins, RepliconRenetPlugins))
        /* channel-endpoint resources, systems */;
        app.run(); // blocks this thread, ticking at ~20 Hz until AppExit
    })?;
```

- **`ScheduleRunnerPlugin::run_loop(wait)` is the fixed cadence**, not a nested
  `FixedUpdate`/`Time<Fixed>` accumulator. Its loop measures `exe_time` per tick and
  sleeps `wait - exe_time`, giving an exact `wait`-length period when a tick fits its
  budget — and if a tick *doesn't* fit, it just runs the next tick immediately with
  **no catch-up multiplier**. Keep sim logic in plain `Update`; stacking `FixedUpdate`
  catch-up on top would reintroduce the exact stall this split exists to avoid (see
  `CHUNK_LOADING_NOTES.md` §0).
- Calling `.run()` off the main thread is fine here specifically because
  `ScheduleRunnerPlugin`'s runner is a plain sleep loop with no OS/platform
  requirement — unlike winit, which needs the real process main thread (Primary's).
- **Shutdown:** an `Arc<AtomicBool>` flag, set by Primary on exit; a system in Sim's
  `Update` checks it and does `exit: MessageWriter<AppExit>` (`AppExit` derives
  `Message`, not the older `Event`); Primary joins the `JoinHandle` afterward.
- **Decided against: nesting Sim as a `NonSendMut<App>` resource inside Primary's
  `World`, pumped manually from a Primary system.** That pattern exists to let a
  non-send-heavy child (window/GPU handles) share a thread with its owner — Sim has
  no non-send state to justify paying that cost. A plain OS thread + channels is
  simpler, and gives Sim's 20 Hz cadence real independence from Primary's frame rate
  instead of coupling their lifecycles.

---

## 4. Cross-world communication

Two real primitives on top of one substrate — not three. ("Manual replication" as a
separate wire mechanism was considered and dropped; see below.)

### The substrate: a generic typed channel

Both worlds' `App`/`SubApp`s exist together on the main thread before Sim's thread
spawns — that's where channels get wired up symmetrically:

```rust
fn cross_world_channel<T: Send + 'static>(from: &mut impl WorldLike, to: &mut impl WorldLike) {
    let (tx, rx) = crossbeam_channel::unbounded::<T>();
    from.insert_resource(ChannelSender(tx));
    to.insert_resource(ChannelReceiver(rx));
}
```

**Direct channels** (the raw substrate, used as-is) are the escape hatch for
bulk/streaming payloads that don't fit event or replication semantics — e.g.
chunk-loading's ready-CPU-mesh channel (`CHUNK_LOADING_NOTES.md` §9). Chunk state in
general is explicitly *not* routed through replication — see decided-against below.

### Events: fire-and-forget

One channel + one generic drain system per event type:
`while let Ok(msg) = rx.try_recv() { writer.write(msg) }`, feeding the destination's
own `MessageWriter<T>` so consumers just use ordinary `MessageReader<T>`. Fits
discrete occurrences: commands (edit, interact, take-item, deal-damage), edit
confirmations, entity spawn/despawn notifications (see below).

### Automatic replication: continuous mirror of selected components

Opt-in per component type (not everything), continuous value sync from an *origin*
entity onto a *shadow* entity in the other world:

- **Identity:** `SimEntityId(Entity)` / `PrimaryEntityId(Entity)` wrap the *origin*
  world's local `Entity` and double as the correlation key. Each world keeps an
  `EntityHashMap<Entity>` from the other side's id to its own local shadow entity —
  mirroring Replicon's own server/client entity-mapping problem, generalized to two
  peer worlds instead of network peers.
- **Direction is per-entity, not per-component.** An entity has exactly one home
  world, so it only ever appears in one of the two maps — "all replicating
  components on an entity go the same direction" falls out of that for free, rather
  than needing separate enforcement.
- Shadow entities carry a back-pointer marker (`OriginSim(SimEntityId)` /
  `OriginPrimary(PrimaryEntityId)`) so their own systems know they're a follower and
  shouldn't replicate back out.
- **Entity lifecycle (spawn/despawn of the origin) is an Event, not a value-diff** —
  it drives shadow creation/teardown and map bookkeeping; component replication only
  runs once the shadow exists.
- Example: a player/camera entity's `Transform`/`ChunkLoader` (radius, LOD)
  replicates Primary → Sim this way — resolves `CHUNK_LOADING_NOTES.md` §2's
  previously-open "loader subscription" mechanism with no bespoke message type.

**Open:** shadow-entity creation trigger — implicit (framework spawns on first
`Added<T>` for a registered component) vs. explicit (`commands.replicate_to_sim(e)`)
is still undecided. Leaning explicit, since it's clearer which entities are meant to
cross the boundary at all.

---

## Decided-against (so these aren't re-litigated after a context reset)

- **`ClientEntityId` / "Client" for either world's identity.** Sim can itself be a
  Replicon client — the word already means something else, and something opposite,
  in this exact system.
- **"Main" for Primary.** Collides with `bevy_app::Main` (a schedule every `App`
  runs, Sim included) and `App::main()`.
- **A third "manual replication" wire primitive**, distinct from Events and
  automatic replication. Collapses into: replicate the value automatically and react
  to `Changed<T>` on the mirrored copy, or use a plain Event if there's no
  persistent entity to hang the change on.
- **Nesting Sim as a `NonSendMut<App>` resource inside Primary.** No non-send state
  in Sim to justify it; a plain thread + channels is simpler and keeps the two
  worlds' cadences genuinely independent.
- **Routing chunk state through generic replication.** Too involved (meshing,
  `Arc`-swap CoW) for diff-and-copy — stays on the direct-channel path
  (`CHUNK_LOADING_NOTES.md` §7, §9).

## Open questions

- Shadow-entity creation trigger: implicit vs. explicit (§4).
