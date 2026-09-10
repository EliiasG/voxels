use bevy::app::{Plugins, ScheduleRunnerPlugin};
use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;
use modul_core::ShouldExit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const TICK: Duration = Duration::from_millis(50); // 20 Hz — from_secs_f64 isn't const

/// Produced by a `SimSetupRecipe`, applied exactly once, on the sim thread, to the
/// `App` that's built there. Unlike `App` itself, this is genuinely `Send` — it
/// only ever closes over plain data (a cloned plugin, a channel `Receiver`), never
/// anything that touches `App`'s non-Send `runner` field, so it can cross the
/// thread boundary with no unsafe impl needed.
type SimSetupStep = Box<dyn FnOnce(&mut App) + Send>;

/// Registered once, wherever a Primary plugin's `build()` calls `add_sim_setup` (or
/// one of the more specific helpers built on it below). `Fn`, not `FnOnce` — a
/// session can begin and end many times per process, so every recipe is replayed
/// fresh on each `begin_sim` rather than consumed once. Runs on Primary's thread
/// (hence `&mut World` here, not `&mut App`) and returns the `SimSetupStep` that
/// finishes the job once the sim thread has an `App` to apply it to.
type SimSetupRecipe = Box<dyn Fn(&mut World) -> SimSetupStep + Send + Sync>;

#[derive(Resource, Default)]
struct SimSetupRecipes(Vec<SimSetupRecipe>);

pub trait SimAppExt {
    /// General entry point: `recipe` runs on Primary each time a session begins and
    /// returns a plain (unboxed) closure — the boxing into a `SimSetupStep` happens
    /// here, once, so callers never write `Box::new(...) as SimSetupStep` by hand.
    fn add_sim_setup<F, S>(&mut self, recipe: F) -> &mut Self
    where
        F: Fn(&mut World) -> S + Send + Sync + 'static,
        S: FnOnce(&mut App) + Send + 'static;

    fn add_sim_plugins<M: 'static>(
        &mut self,
        plugins: impl Plugins<M> + Clone + Send + Sync + 'static,
    ) -> &mut Self;
}

impl SimAppExt for App {
    fn add_sim_setup<F, S>(&mut self, recipe: F) -> &mut Self
    where
        F: Fn(&mut World) -> S + Send + Sync + 'static,
        S: FnOnce(&mut App) + Send + 'static,
    {
        self.world_mut()
            .get_resource_or_insert_with(SimSetupRecipes::default)
            .0
            .push(Box::new(move |primary_world| {
                Box::new(recipe(primary_world)) as SimSetupStep
            }));
        self
    }

    fn add_sim_plugins<M: 'static>(
        &mut self,
        plugins: impl Plugins<M> + Clone + Send + Sync + 'static,
    ) -> &mut Self {
        self.add_sim_setup(move |_primary_world: &mut World| {
            let plugins = plugins.clone();
            move |sim: &mut App| {
                sim.add_plugins(plugins);
            }
        })
    }
}

#[derive(Resource)]
struct ChannelSender<T>(crossbeam_channel::Sender<T>);
#[derive(Resource)]
struct ChannelReceiver<T>(crossbeam_channel::Receiver<T>);

// `Option<Res<_>>`: both systems are added to Primary's schedule as soon as the
// event type is registered, but the channel resource they read only exists once a
// sim session has actually begun (possibly never, e.g. sitting at a main menu).
fn forward_out<T: Message + Clone>(mut reader: MessageReader<T>, tx: Option<Res<ChannelSender<T>>>) {
    let Some(tx) = tx else { return };
    for msg in reader.read() {
        let _ = tx.0.send(msg.clone());
    }
}

fn forward_in<T: Message + Send + Sync + 'static>(
    rx: Option<Res<ChannelReceiver<T>>>,
    mut writer: MessageWriter<T>,
) {
    let Some(rx) = rx else { return };
    while let Ok(msg) = rx.0.try_recv() {
        writer.write(msg);
    }
}

pub trait CrossWorldEventExt {
    fn add_event_to_sim<T: Message + Clone>(&mut self) -> &mut Self;
    fn add_event_from_sim<T: Message + Clone>(&mut self) -> &mut Self;
}

impl CrossWorldEventExt for App {
    fn add_event_to_sim<T: Message + Clone>(&mut self) -> &mut Self {
        self.add_message::<T>();
        self.add_systems(Last, forward_out::<T>);
        self.add_sim_setup(|primary_world: &mut World| {
            let (tx, rx) = crossbeam_channel::unbounded::<T>();
            primary_world.insert_resource(ChannelSender(tx));
            move |sim_app: &mut App| {
                sim_app.add_message::<T>();
                sim_app.insert_resource(ChannelReceiver(rx));
                sim_app.add_systems(First, forward_in::<T>);
            }
        })
    }

    fn add_event_from_sim<T: Message + Clone>(&mut self) -> &mut Self {
        self.add_message::<T>();
        self.add_systems(First, forward_in::<T>);
        self.add_sim_setup(|primary_world: &mut World| {
            let (tx, rx) = crossbeam_channel::unbounded::<T>();
            primary_world.insert_resource(ChannelReceiver(rx));
            move |sim_app: &mut App| {
                sim_app.add_message::<T>();
                sim_app.insert_resource(ChannelSender(tx));
                sim_app.add_systems(Last, forward_out::<T>);
            }
        })
    }
}

// -------------------------------------------------------------------------------
// Replication: continuous mirror of opted-in entities/components between the two
// worlds. Everything here uses standard bevy_app schedules (`PostUpdate` to
// detect, `PreUpdate` to apply) rather than a bespoke one, on purpose — Primary
// will run these same labels for real once `MainSchedulePlugin` is bridged into
// modul's `Redraw` (not done yet; tracked in ARCHITECTURE_NOTES.md), and Sim
// already does via `MinimalPlugins`. Detect in `PostUpdate` so it sees changes
// gameplay code made in `Update` this same tick (Commands from `Update` are
// flushed by the time `PostUpdate` starts); apply in `PreUpdate` so shadow state
// is in place before the receiving world's own `Update` runs.

/// Correlates an origin entity with its shadow. The wrapped `Entity` is always the
/// *origin* world's local id — meaningless to dereference in the world that
/// receives it, only used as a map key there.
#[derive(Component, Clone, Copy)]
pub struct PrimaryEntityId(pub Entity);
#[derive(Component, Clone, Copy)]
pub struct SimEntityId(pub Entity);

/// Back-pointer on a shadow entity to its origin, so a shadow's own systems can
/// recognize it's a follower rather than a source of truth.
#[derive(Component)]
pub struct OriginPrimary(pub PrimaryEntityId);
#[derive(Component)]
pub struct OriginSim(pub SimEntityId);

/// Lives on Sim: Primary's local `Entity` -> the shadow Sim spawned for it.
#[derive(Resource, Default)]
struct PrimaryShadowMap(EntityHashMap<Entity>);
/// Lives on Primary: Sim's local `Entity` -> the shadow Primary spawned for it.
#[derive(Resource, Default)]
struct SimShadowMap(EntityHashMap<Entity>);

/// Explicit opt-in, mirroring Replicon's own `Replicated` marker: an entity only
/// gets a shadow, and only has its opted-in components synced, while it carries
/// this. Direction is per-entity — carry at most one of these two.
#[derive(Component)]
pub struct ReplicateToSim;
#[derive(Component)]
pub struct ReplicateToPrimary;

#[derive(Message, Clone, Copy)]
struct PrimaryEntitySpawned(PrimaryEntityId);
#[derive(Message, Clone, Copy)]
struct PrimaryEntityDespawned(PrimaryEntityId);
#[derive(Message, Clone, Copy)]
struct SimEntitySpawned(SimEntityId);
#[derive(Message, Clone, Copy)]
struct SimEntityDespawned(SimEntityId);

/// Whole value on every change — not a diff. `origin` is the origin world's local
/// `Entity`, the lookup key into the receiving side's shadow map.
#[derive(Message, Clone)]
struct ComponentUpdate<C> {
    origin: Entity,
    value: C,
}

/// Orders shadow creation/removal before component values are applied, within the
/// same `PreUpdate`. Both are commonly true in the same tick — an entity is
/// usually marked *and* populated together — and a value applied before its shadow
/// exists would be silently dropped (the map lookup just misses). Configured in
/// every registration function below, redundantly but harmlessly, so the ordering
/// holds regardless of which order `enable_replication_to_*` /
/// `replicate_component_to_*` are called in.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ReplicationSet {
    ApplyLifecycle,
    ApplyValues,
}

fn detect_primary_spawns(q: Query<Entity, Added<ReplicateToSim>>, mut w: MessageWriter<PrimaryEntitySpawned>) {
    for e in &q {
        w.write(PrimaryEntitySpawned(PrimaryEntityId(e)));
    }
}

// `RemovedComponents<ReplicateToSim>` also fires when the whole entity is
// despawned, not just on an explicit `.remove::<ReplicateToSim>()` — despawning
// counts as removing every component the entity had.
fn detect_primary_despawns(
    mut removed: RemovedComponents<ReplicateToSim>,
    mut w: MessageWriter<PrimaryEntityDespawned>,
) {
    for e in removed.read() {
        w.write(PrimaryEntityDespawned(PrimaryEntityId(e)));
    }
}

fn spawn_primary_shadows(
    mut r: MessageReader<PrimaryEntitySpawned>,
    mut map: ResMut<PrimaryShadowMap>,
    mut commands: Commands,
) {
    for &PrimaryEntitySpawned(id) in r.read() {
        let shadow = commands.spawn(OriginPrimary(id)).id();
        map.0.insert(id.0, shadow);
    }
}

fn despawn_primary_shadows(
    mut r: MessageReader<PrimaryEntityDespawned>,
    mut map: ResMut<PrimaryShadowMap>,
    mut commands: Commands,
) {
    for &PrimaryEntityDespawned(id) in r.read() {
        if let Some(shadow) = map.0.remove(&id.0) {
            commands.entity(shadow).despawn();
        }
    }
}

fn detect_sim_spawns(q: Query<Entity, Added<ReplicateToPrimary>>, mut w: MessageWriter<SimEntitySpawned>) {
    for e in &q {
        w.write(SimEntitySpawned(SimEntityId(e)));
    }
}

fn detect_sim_despawns(
    mut removed: RemovedComponents<ReplicateToPrimary>,
    mut w: MessageWriter<SimEntityDespawned>,
) {
    for e in removed.read() {
        w.write(SimEntityDespawned(SimEntityId(e)));
    }
}

fn spawn_sim_shadows(
    mut r: MessageReader<SimEntitySpawned>,
    mut map: ResMut<SimShadowMap>,
    mut commands: Commands,
) {
    for &SimEntitySpawned(id) in r.read() {
        let shadow = commands.spawn(OriginSim(id)).id();
        map.0.insert(id.0, shadow);
    }
}

fn despawn_sim_shadows(
    mut r: MessageReader<SimEntityDespawned>,
    mut map: ResMut<SimShadowMap>,
    mut commands: Commands,
) {
    for &SimEntityDespawned(id) in r.read() {
        if let Some(shadow) = map.0.remove(&id.0) {
            commands.entity(shadow).despawn();
        }
    }
}

// `Or<(Changed<C>, Added<ReplicateToSim>)>`: `Changed<C>` alone misses an entity
// whose data was set *before* it was marked and never changes again — the shadow
// would spawn and then sit empty forever, since nothing would ever "change" again
// to trigger a send. Catching `Added<ReplicateToSim>` too guarantees a value is
// sent the instant an entity opts in, using whatever `C` holds at that moment.
#[allow(clippy::type_complexity)] // query filter, not worth a type alias
fn detect_component_to_sim<C: Component + Clone>(
    mut w: MessageWriter<ComponentUpdate<C>>,
    q: Query<(Entity, &C), (With<ReplicateToSim>, Or<(Changed<C>, Added<ReplicateToSim>)>)>,
) {
    for (e, v) in &q {
        w.write(ComponentUpdate { origin: e, value: v.clone() });
    }
}

fn apply_component_from_primary<C: Component + Clone>(
    mut r: MessageReader<ComponentUpdate<C>>,
    map: Option<Res<PrimaryShadowMap>>,
    mut commands: Commands,
) {
    let Some(map) = map else { return };
    for update in r.read() {
        if let Some(&shadow) = map.0.get(&update.origin) {
            commands.entity(shadow).insert(update.value.clone());
        }
    }
}

#[allow(clippy::type_complexity)] // query filter, not worth a type alias
fn detect_component_to_primary<C: Component + Clone>(
    mut w: MessageWriter<ComponentUpdate<C>>,
    q: Query<(Entity, &C), (With<ReplicateToPrimary>, Or<(Changed<C>, Added<ReplicateToPrimary>)>)>,
) {
    for (e, v) in &q {
        w.write(ComponentUpdate { origin: e, value: v.clone() });
    }
}

fn apply_component_from_sim<C: Component + Clone>(
    mut r: MessageReader<ComponentUpdate<C>>,
    map: Option<Res<SimShadowMap>>,
    mut commands: Commands,
) {
    let Some(map) = map else { return };
    for update in r.read() {
        if let Some(&shadow) = map.0.get(&update.origin) {
            commands.entity(shadow).insert(update.value.clone());
        }
    }
}

pub trait ReplicateExt {
    /// One-time setup for a direction: entities carrying `ReplicateToSim` get a
    /// Sim shadow, created and torn down as the marker (or the whole entity) is
    /// added/removed. Call once; components are then opted in individually via
    /// `replicate_component_to_sim`.
    fn enable_replication_to_sim(&mut self) -> &mut Self;
    fn enable_replication_to_primary(&mut self) -> &mut Self;

    /// Continuous one-way mirror of `C` from an origin entity onto its shadow.
    /// Whole-value, not a diff. The matching `enable_replication_to_*` needs to be
    /// called too (either order — the apply-ordering constraint is declared here
    /// as well, not only there).
    fn replicate_component_to_sim<C: Component + Clone>(&mut self) -> &mut Self;
    fn replicate_component_to_primary<C: Component + Clone>(&mut self) -> &mut Self;
}

impl ReplicateExt for App {
    fn enable_replication_to_sim(&mut self) -> &mut Self {
        self.add_event_to_sim::<PrimaryEntitySpawned>();
        self.add_event_to_sim::<PrimaryEntityDespawned>();
        self.add_systems(PostUpdate, (detect_primary_spawns, detect_primary_despawns));
        self.add_sim_setup(|_primary_world: &mut World| {
            move |sim: &mut App| {
                sim.init_resource::<PrimaryShadowMap>();
                sim.configure_sets(
                    PreUpdate,
                    ReplicationSet::ApplyLifecycle.before(ReplicationSet::ApplyValues),
                );
                sim.add_systems(
                    PreUpdate,
                    (spawn_primary_shadows, despawn_primary_shadows).in_set(ReplicationSet::ApplyLifecycle),
                );
            }
        })
    }

    fn enable_replication_to_primary(&mut self) -> &mut Self {
        self.add_event_from_sim::<SimEntitySpawned>();
        self.add_event_from_sim::<SimEntityDespawned>();
        self.init_resource::<SimShadowMap>();
        self.configure_sets(
            PreUpdate,
            ReplicationSet::ApplyLifecycle.before(ReplicationSet::ApplyValues),
        );
        self.add_systems(
            PreUpdate,
            (spawn_sim_shadows, despawn_sim_shadows).in_set(ReplicationSet::ApplyLifecycle),
        );
        self.add_sim_setup(|_primary_world: &mut World| {
            move |sim: &mut App| {
                sim.add_systems(PostUpdate, (detect_sim_spawns, detect_sim_despawns));
            }
        })
    }

    fn replicate_component_to_sim<C: Component + Clone>(&mut self) -> &mut Self {
        self.add_event_to_sim::<ComponentUpdate<C>>();
        self.add_systems(PostUpdate, detect_component_to_sim::<C>);
        self.add_sim_setup(|_primary_world: &mut World| {
            move |sim: &mut App| {
                sim.configure_sets(
                    PreUpdate,
                    ReplicationSet::ApplyLifecycle.before(ReplicationSet::ApplyValues),
                );
                sim.add_systems(
                    PreUpdate,
                    apply_component_from_primary::<C>.in_set(ReplicationSet::ApplyValues),
                );
            }
        })
    }

    fn replicate_component_to_primary<C: Component + Clone>(&mut self) -> &mut Self {
        self.add_event_from_sim::<ComponentUpdate<C>>();
        self.configure_sets(
            PreUpdate,
            ReplicationSet::ApplyLifecycle.before(ReplicationSet::ApplyValues),
        );
        self.add_systems(
            PreUpdate,
            apply_component_from_sim::<C>.in_set(ReplicationSet::ApplyValues),
        );
        self.add_sim_setup(|_primary_world: &mut World| {
            move |sim: &mut App| {
                sim.add_systems(PostUpdate, detect_component_to_primary::<C>);
            }
        })
    }
}

#[derive(Message, Clone, Copy, Default)]
pub struct BeginSim;

pub struct SimPlugin;

impl Plugin for SimPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BeginSim>();
        app.add_systems(Update, (handle_begin_sim, watch_primary_exit));
    }
}

fn handle_begin_sim(mut reader: MessageReader<BeginSim>, mut commands: Commands) {
    for _ in reader.read() {
        commands.queue(|world: &mut World| begin_sim(world));
    }
}

fn watch_primary_exit(should_exit: Option<Res<ShouldExit>>, mut commands: Commands) {
    if should_exit.is_some() {
        commands.remove_resource::<SimHandle>();
    }
}

pub fn begin_sim(primary_world: &mut World) {
    if primary_world.contains_resource::<SimHandle>() {
        eprintln!("[sim] a session is already running — replacing it");
    }
    println!("[sim] starting sim session");

    // Recipes are created lazily by add_sim_setup and friends; ensure the resource
    // exists even if nothing ever registered one, so resource_scope doesn't panic on
    // an otherwise-valid, plugin-less sim. Each recipe runs here, on Primary's
    // thread, while it still has `&mut World`; what it returns is the
    // `SimSetupStep` that finishes the job later, on the sim thread.
    primary_world.get_resource_or_insert_with(SimSetupRecipes::default);
    let steps: Vec<SimSetupStep> = primary_world.resource_scope::<SimSetupRecipes, _>(|primary_world, recipes| {
        recipes.0.iter().map(|recipe| recipe(primary_world)).collect()
    });

    let shutdown = Arc::new(AtomicBool::new(false));
    let sim_shutdown = shutdown.clone();

    // `App` itself never crosses this boundary — only `steps` (plain, genuinely
    // `Send` data) and `sim_shutdown` do. The `App` is built fresh here, on the sim
    // thread, so there's no need to assert `Send` on it at all.
    let join_handle = thread::Builder::new()
        .name("sim-world".into())
        .spawn(move || {
            let mut sim_app = App::new();
            sim_app.add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(TICK)));

            for step in steps {
                step(&mut sim_app);
            }

            sim_app.insert_resource(SimShutdownFlag(sim_shutdown));
            sim_app.add_systems(Update, sim_shutdown_watcher);

            sim_app.run();
        })
        .expect("failed to spawn sim-world thread");

    primary_world.insert_resource(SimHandle {
        join_handle: Some(join_handle),
        shutdown,
    });
}

#[derive(Resource, Clone)]
struct SimShutdownFlag(Arc<AtomicBool>);

fn sim_shutdown_watcher(flag: Res<SimShutdownFlag>, mut exit: MessageWriter<AppExit>) {
    if flag.0.load(Ordering::Relaxed) {
        exit.write(AppExit::Success);
    }
}

#[derive(Resource)]
pub struct SimHandle {
    join_handle: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for SimHandle {
    // Blocks the caller for up to one Sim tick. Deliberate: guarantees the old
    // session has released its resources (network socket, Replicon state) before
    // whatever replaced this handle — end of session, or a second begin_sim —
    // proceeds.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join_handle.take()
            && let Err(panic) = handle.join()
        {
            eprintln!("[sim] sim thread panicked: {panic:?}");
        }
    }
}
