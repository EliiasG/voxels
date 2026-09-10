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
- **Planned, not yet done: bridge `bevy_app`'s standard schedules onto `Redraw`.**
  Today, `SubApp::new()` (what modul builds Primary on) never gets
  `bevy_app::MainSchedulePlugin` added — only `App::new()` does that automatically —
  so `First`/`PreUpdate`/`Update`/`PostUpdate`/`Last` exist as labels but nothing
  ever runs them on Primary; a Primary plugin writing `add_systems(Update, sys)`
  today silently no-ops forever. Fix, without touching modul itself: add
  `sub_app.add_plugins(MainSchedulePlugin)` once in voxels' own Primary setup, then
  register one system in `Redraw` whose body is `world.run_schedule(Main)` (`Main`'s
  own system already chains the standard sub-schedules via `MainScheduleOrder`, once
  `MainSchedulePlugin` has registered it). After that, Primary and Sim share the same
  schedule vocabulary — `Update` means "once per `Redraw`" on Primary and "once per
  tick" on Sim, different cadence, same label. `PreInit`/`Init`/`Redraw` stay modul's
  own bootstrap/display-pacing schedules regardless — no bevy_app equivalent, not
  meant to be replaced. `src/sim.rs`'s replication code (§4) is already written
  assuming this bridge exists, using standard schedules throughout.

---

## 3. Sim world: ownership & threading

Sim has no windowing/GPU state at all, so unlike Primary it doesn't need to live on
any particular thread for platform reasons — it's a genuine standalone `App`, built
*inside* the `thread::spawn` closure, not before it. The whole mechanism lives in
**`src/sim.rs`**:

- **Papercut driving the split below: `App` is not `Send` in this bevy version**,
  regardless of which runner is set — its `runner` field is `Box<dyn FnOnce(App) ->
  AppExit>` with no `+Send` bound, so the auto-trait check fails structurally. That
  rules out building `sim_app` on Primary's thread (where it'd need `&mut World` to
  register channels etc., per §4) and then moving the finished `App` into the
  thread — so `begin_sim` doesn't do that.
- **Two-phase recipes instead.** `SimSetupRecipe = Fn(&mut World) -> SimSetupStep`
  runs on Primary's thread, where it still has `&mut World` — this is where
  `add_event_to_sim`/`add_event_from_sim` create the channel and keep whichever end
  (`Sender`/`Receiver`) belongs on Primary. What it returns, `SimSetupStep = Box<dyn
  FnOnce(&mut App) + Send>`, is the part that's actually sent across: a one-shot
  closure closing only over plain, genuinely-`Send` data (a cloned plugin, a channel
  end) — never anything that touches `App`'s non-Send `runner` field. `begin_sim`
  collects every registered recipe's step before spawning, then the spawned closure
  builds a fresh `App` and applies each one to it. No `unsafe impl Send` anywhere.
- **`SimAppExt::add_sim_setup`** is the general entry point — `recipe` runs on
  Primary each session and returns a plain, unboxed closure; the boxing into a
  `SimSetupStep` happens once, inside `add_sim_setup` itself, so nothing else has to
  write `Box::new(...) as SimSetupStep` by hand. `add_sim_plugins` and
  `add_event_to_sim`/`add_event_from_sim` are all just callers of it. Backed by a
  `SimSetupRecipes` resource on Primary's `World`: a list of the two-phase recipes
  above, `Fn` not `FnOnce` and replayed (not consumed) on every session, since Sim
  can begin and end many times per process, not just once at startup.
- **`BeginSim` (message) / `begin_sim(world: &mut World)`** — `BeginSim` is the
  ordinary-system entry point (`MessageWriter<BeginSim>`, e.g. from a "New Game"
  button); `SimPlugin`'s `handle_begin_sim` system drains it via `Commands::queue`
  into `begin_sim`, which runs every recipe, then spawns the thread. The spawned
  closure itself builds the `App` (currently just `MinimalPlugins` +
  `ScheduleRunnerPlugin`), applies every `SimSetupStep`, and runs it. Anything else a
  session needs — Replicon, `StatesPlugin`, game content — is deliberately not
  hardcoded here anymore; it's `add_sim_plugins`'s job, called from wherever Primary
  assembles its own plugin list (not wired up yet — `main.rs` doesn't build Primary's
  plugin list at all yet).
- **`SimHandle`** (`Resource` on Primary) owns the `JoinHandle` and the shutdown
  flag; its `Drop` impl sets the flag and joins, so tearing down a session — window
  close (`watch_primary_exit`, below) or a second `begin_sim` overwriting the old
  handle — is just "stop holding the resource." The join blocks the caller for up to
  one Sim tick; deliberate, so a torn-down session's resources (network socket,
  Replicon state) are actually released before whatever replaced it proceeds.
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

Implemented in `src/sim.rs` (`ReplicateExt`). Opt-in per component type (not
everything), continuous value sync from an *origin* entity onto a *shadow* entity in
the other world — built entirely on top of the Events substrate above, not a
separate mechanism:

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
- **Shadow-entity creation trigger: resolved, explicit** — an entity opts in by
  carrying a `ReplicateToSim`/`ReplicateToPrimary` marker component, mirroring
  Replicon's own `Replicated` marker. `enable_replication_to_sim`/
  `enable_replication_to_primary` wire the lifecycle once per direction: `Added<the
  marker>` → a spawn message → the shadow is created on the other side;
  `RemovedComponents<the marker>` → a despawn message → the shadow is torn down.
  `RemovedComponents` also fires on the whole entity being despawned, not just an
  explicit `.remove()` — despawning counts as removing everything the entity had, so
  no separate despawn-of-whole-entity case is needed.
- **Component value sync reuses the event channel, generically.** Rather than a
  bespoke diff mechanism, a component update is just another message:
  `ComponentUpdate<C> { origin: Entity, value: C }`, sent whole (not diffed) through
  the same `add_event_to_sim`/`add_event_from_sim` plumbing. `replicate_component_to_
  sim::<C>`/`replicate_component_to_primary::<C>` register one pair of detect/apply
  systems per opted-in component type.
- **Detect trigger is `Or<(Changed<C>, Added<the marker>)>`, not just `Changed<C>`.**
  `Changed<C>` alone misses an entity whose data was set *before* it was marked and
  never changes again — the shadow would spawn and then sit empty forever, since
  nothing would ever "change" again to send it. Catching the marker's own `Added`
  guarantees a value goes out the instant an entity opts in, regardless of when its
  data was set relative to that.
- **Ordering: detect in `PostUpdate`, apply in `PreUpdate`.** Both the shadow's
  creation and its first value update are commonly true in the *same* tick (an
  entity is usually marked and populated together) — a value applied before its
  shadow exists would silently no-op (the map lookup just misses). Detecting in
  `PostUpdate` means gameplay's `Update`-stage Commands (e.g. the spawn itself) are
  already flushed by the time detection runs; applying in `PreUpdate` puts the
  shadow in place before the receiving world's own `Update` runs. Within `PreUpdate`,
  a `ReplicationSet` (`ApplyLifecycle` before `ApplyValues`) — configured redundantly
  in every registration function, so it holds no matter which order
  `enable_replication_to_*`/`replicate_component_to_*` are called in — guarantees
  shadow creation actually happens before that shadow's values are applied, even
  though both go through deferred `Commands` (which apply in queue order, so
  system-order is sufficient — no explicit `apply_deferred` needed between them).
  This only works because both worlds run the standard `bevy_app` schedule
  vocabulary — see §2's `MainSchedulePlugin` bridge note for why that isn't yet true
  of Primary.
- Example: a player/camera entity's `Transform`/`ChunkLoader` (radius, LOD)
  replicates Primary → Sim this way — resolves `CHUNK_LOADING_NOTES.md` §2's
  previously-open "loader subscription" mechanism with no bespoke message type.
- **Not yet handled:** removing a single opted-in component without despawning the
  whole entity (the shadow keeps a stale value); real diffing (every change resends
  the whole value); two worlds both claiming the same entity (sidestepped by
  direction being per-entity, not enforced beyond that).

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

- None currently — shadow-entity creation trigger (§4) was the last one, now
  resolved and implemented.
