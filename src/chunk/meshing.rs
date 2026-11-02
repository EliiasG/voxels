use crate::chunk::manager::{ChunkPriority, ChunkReference};
use crate::chunk::{ChunkData, ChunkEntity, ChunkNeighbors, ChunkPosition, ChunkStorage};
use bevy::math::IVec3;
use bevy::prelude::*;
use bytemuck::{Pod, Zeroable};
use std::marker::PhantomData;
use std::sync::Arc;

const MAX_MESHES_POPPED_PER_TICK: usize = 2048;

/// Relative positions of neighbors needed for [ChunkMesherManager::schedule_meshing]:
/// all 26 neighbors except the 8 corners. Same order as the neighbors passed to it.
pub const CHUNK_MESH_NEIGHBORS: [IVec3; 18] = {
    let mut neighbors = [IVec3::ZERO; 18];
    let mut count = 0;
    let mut i: i32 = 0;
    while i < 27 {
        let (x, y, z) = (i % 3 - 1, i / 3 % 3 - 1, i / 9 - 1);
        let distance = x.abs() + y.abs() + z.abs();
        // faces are 1 away, edges 2, corners 3 (and the chunk itself 0)
        if distance == 1 || distance == 2 {
            neighbors[count] = IVec3::new(x, y, z);
            count += 1;
        }
        i += 1;
    }
    assert!(count == 18);
    neighbors
};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct FaceData {
    x: u8,
    y: u8,
    z: u8,
    w: u8,
    h: u8,
    /// 4 corners * 2 bits
    ao: u8,
    material: u16,
}

/// Per-direction face data: standard faces (always drawn) + border faces
/// (only drawn when same-LOD neighbor in this direction is hidden by finer LOD).
pub struct DirFaces {
    pub standard: Vec<FaceData>,
    pub border: Vec<FaceData>,
}

pub struct ChunkMesh(pub [DirFaces; 6]);

#[derive(Component, Default)]
pub struct ChunkMeshComponent {
    mesh: Option<Arc<ChunkMesh>>,
    /// Whether [Self::mesh] came from [ChunkMesherManager::schedule_meshing] and not the early function
    full: bool,
}

impl ChunkMeshComponent {
    pub fn mesh(&self) -> Option<&Arc<ChunkMesh>> {
        self.mesh.as_ref()
    }
}

pub struct ChunkMesherOutput {
    pub chunk: Arc<ChunkMesh>,
    /// Corresponding chunk entity for mesh
    pub entity: ChunkEntity,
    /// If it was generated form the early function
    pub early: bool,
}

pub trait ChunkMesherManager {
    /// Neighbors are in the order of [CHUNK_MESH_NEIGHBORS], `None` where there is no chunk or it has no data yet
    // All possible neighbors except the 8 corners, needed for AO
    fn schedule_meshing(
        &mut self,
        chunk_ref: ChunkReference,
        chunk_data: Arc<ChunkStorage>,
        neighbors: [Option<Arc<ChunkStorage>>; 18],
    );

    // To get a rough mesh quickly
    fn schedule_early_meshing(&mut self, chunk_ref: ChunkReference, chunk_data: Arc<ChunkStorage>);

    fn pop(&mut self) -> Option<ChunkMesherOutput>;
}

#[derive(Resource)]
pub struct ChunkMesherResource<Mesher: ChunkMesherManager>(pub Mesher);

pub struct ChunkMeshingPlugin<M: ChunkMesherManager> {
    pd: PhantomData<M>,
}

impl<M: ChunkMesherManager> ChunkMeshingPlugin<M> {
    pub fn new() -> Self {
        Self { pd: PhantomData }
    }
}

impl<M: ChunkMesherManager + Send + Sync + 'static> Plugin for ChunkMeshingPlugin<M> {
    fn build(&self, app: &mut App) {
        // Neighbors are linked in PreUpdate (see ChunkManagerPlugin), so no ordering against that is needed.
        app.add_systems(
            Update,
            (
                // the order matters: the deltas and recount read the state the commit overwrites,
                // and meshes are scheduled from the finished counts
                (
                    apply_neighbor_deltas,
                    recount_own_neighbors,
                    commit_counted_state,
                    schedule_ready_meshes::<M>,
                )
                    .chain(),
                schedule_early_meshes::<M>,
                pull_meshes::<M>,
            ),
        );
    }
}

/// Tracks how many chunk neighbors [CHUNK_MESH_NEIGHBORS] of same or less value (more urgent) are spawned but w/o data.
///
/// A chunk is ready to be meshed when this is 0 and it has data. Neighbors of lower urgency are not waited for:
/// they might not be generated for a long time. Instead, chunks that were meshed without a neighbor's data
/// are remeshed when it arrives (see [MeshStatus]).
#[derive(Component, Clone, Copy, Default, PartialEq)]
pub struct MissingNeighborCount(i32);

/// What the neighbors' [MissingNeighborCount]s currently account for from this chunk.
/// Written at the end of the counting systems, so while they run it holds the state before this frame's changes.
#[derive(Component, Clone, Copy, Default, PartialEq)]
pub struct CountedState {
    /// `None` until the chunk has been counted for the first time
    priority: Option<ChunkPriority>,
    has_data: bool,
}

/// A chunk blocks a neighbor of priority `neighbor_priority` from meshing if it is spawned but w/o data
/// and at least as urgent.
#[inline]
fn blocks(priority: Option<ChunkPriority>, has_data: bool, neighbor_priority: ChunkPriority) -> bool {
    !has_data && priority.is_some_and(|priority| priority <= neighbor_priority)
}

/// Not a marker component to avoid moving chunks between archetypes
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum MeshStatus {
    #[default]
    Unmeshed,
    /// Only one mesh job per chunk at a time, so an older mesh can't overwrite a newer one.
    /// `dirty` if a neighbor's data arrived after the job was scheduled.
    InFlight { dirty: bool },
    Meshed,
    /// Meshed, but a neighbor's data arrived after the job was scheduled, so it should be meshed again
    Stale,
}

impl MeshStatus {
    /// Called when a neighbor's data arrives
    fn neighbor_data_arrived(self) -> Self {
        match self {
            MeshStatus::InFlight { .. } => MeshStatus::InFlight { dirty: true },
            MeshStatus::Meshed => MeshStatus::Stale,
            other => other,
        }
    }
}

type ChunkCountChanges = Or<(Changed<ChunkPriority>, Changed<ChunkData>)>;

/// Step 1 of the counting: every chunk whose priority or data changed updates the counts of the neighbors
/// that don't recount (unchanged priority) by what its change means for them.
///
/// Changes of a chunk D to a neighbor N's count (D blocks N if [blocks]):
/// - D spawned: +1 if blocks
/// - D's priority upgraded (it has no data): +1 if it blocks now but did not before
/// - D's data arrived: -1 if it blocked
/// - D despawned: -1 if it blocked, see [release_chunk]
///
/// Also marks neighbors that were meshed without D's data for remeshing.
fn apply_neighbor_deltas(
    changed: Query<
        (
            Ref<ChunkData>,
            &ChunkPriority,
            &CountedState,
            &ChunkNeighbors,
        ),
        ChunkCountChanges,
    >,
    mut neighbors: Query<(
        &ChunkPriority,
        &CountedState,
        &mut MissingNeighborCount,
        &mut MeshStatus,
    )>,
) {
    for (data, &priority, &counted, chunk_neighbors) in &changed {
        let has_data = data.0.is_some();
        if !has_data && data.is_changed() && !data.is_added() {
            // chunk data is only ever set
            eprintln!("chunk data changed to none");
        }
        if counted.priority == Some(priority) && counted.has_data == has_data {
            // flagged as changed without actually changing
            continue;
        }
        let data_arrived = has_data && !counted.has_data;
        for relative_pos in CHUNK_MESH_NEIGHBORS {
            let Some(neighbor) = chunk_neighbors.neighbor(relative_pos) else {
                continue;
            };
            let Ok((neighbor_priority, neighbor_counted, mut count, mut status)) =
                neighbors.get_mut(neighbor.0)
            else {
                continue;
            };
            // a neighbor with changed priority counts all its neighbors again, and will see this chunk's current state
            if neighbor_counted.priority == Some(*neighbor_priority) {
                let delta = blocks(Some(priority), has_data, *neighbor_priority) as i32
                    - blocks(counted.priority, counted.has_data, *neighbor_priority) as i32;
                if delta != 0 {
                    count.0 += delta;
                }
            }
            if data_arrived {
                let next = status.neighbor_data_arrived();
                status.set_if_neq(next);
            }
        }
    }
}

/// Step 2 of the counting: every chunk with a changed priority (this includes new chunks) sets its own count
/// from scratch, from the current state of its neighbors.
fn recount_own_neighbors(
    changed: Query<
        (Entity, &ChunkPriority, &CountedState, &ChunkNeighbors),
        Changed<ChunkPriority>,
    >,
    neighbor_info: Query<(&ChunkPriority, &ChunkData)>,
    mut counts: Query<&mut MissingNeighborCount>,
) {
    for (entity, &priority, counted, chunk_neighbors) in &changed {
        if counted.priority == Some(priority) {
            continue;
        }
        let missing = CHUNK_MESH_NEIGHBORS
            .iter()
            .filter_map(|&relative_pos| chunk_neighbors.neighbor(relative_pos))
            .filter(|neighbor| {
                neighbor_info.get(neighbor.0).is_ok_and(|(neighbor_priority, data)| {
                    blocks(Some(*neighbor_priority), data.0.is_some(), priority)
                })
            })
            .count();
        if let Ok(mut count) = counts.get_mut(entity) {
            count.set_if_neq(MissingNeighborCount(missing as i32));
        }
    }
}

/// Step 3 of the counting: the counts now account for the current state of every chunk
fn commit_counted_state(
    mut changed: Query<(Ref<ChunkData>, &ChunkPriority, &mut CountedState), ChunkCountChanges>,
) {
    for (data, &priority, mut counted) in &mut changed {
        counted.set_if_neq(CountedState {
            priority: Some(priority),
            has_data: data.0.is_some(),
        });
    }
}

/// Schedules meshing of chunks that have all the neighbor data they wait for
fn schedule_ready_meshes<M: ChunkMesherManager + Send + Sync + 'static>(
    mut mesher: ResMut<ChunkMesherResource<M>>,
    mut ready: Query<
        (
            Entity,
            &ChunkPosition,
            &ChunkData,
            &MissingNeighborCount,
            &ChunkNeighbors,
            &mut MeshStatus,
        ),
        Or<(
            Changed<MissingNeighborCount>,
            Changed<ChunkData>,
            Changed<MeshStatus>,
        )>,
    >,
    data_query: Query<&ChunkData>,
) {
    for (entity, position, data, count, chunk_neighbors, mut status) in &mut ready {
        let Some(storage) = &data.0 else {
            continue;
        };
        if !matches!(*status, MeshStatus::Unmeshed | MeshStatus::Stale) {
            continue;
        }
        if count.0 < 0 {
            eprintln!("negative missing neighbor count");
        }
        if count.0 > 0 {
            continue;
        }
        let neighbors = CHUNK_MESH_NEIGHBORS.map(|relative_pos| {
            let neighbor = chunk_neighbors.neighbor(relative_pos)?;
            data_query.get(neighbor.0).ok()?.0.clone()
        });
        mesher.0.schedule_meshing(
            ChunkReference {
                position: position.0,
                entity: ChunkEntity(entity),
            },
            storage.clone(),
            neighbors,
        );
        *status = MeshStatus::InFlight { dirty: false };
    }
}

/// Gets a rough mesh of newly generated chunks quickly
fn schedule_early_meshes<M: ChunkMesherManager + Send + Sync + 'static>(
    mut mesher: ResMut<ChunkMesherResource<M>>,
    new_chunks: Query<(Entity, &ChunkPosition, &ChunkData), Changed<ChunkData>>,
) {
    for (entity, position, data) in &new_chunks {
        // spawned chunks have no data yet
        let Some(storage) = &data.0 else {
            continue;
        };
        mesher.0.schedule_early_meshing(
            ChunkReference {
                position: position.0,
                entity: ChunkEntity(entity),
            },
            storage.clone(),
        );
    }
}

fn pull_meshes<M: ChunkMesherManager + Send + Sync + 'static>(
    mut mesher: ResMut<ChunkMesherResource<M>>,
    mut chunks: Query<(&mut ChunkMeshComponent, &mut MeshStatus)>,
) {
    for _ in 0..MAX_MESHES_POPPED_PER_TICK {
        let Some(ChunkMesherOutput { chunk, entity, early }) = mesher.0.pop() else {
            break;
        };
        let Ok((mut mesh, mut status)) = chunks.get_mut(entity.0) else {
            // chunk might unload, but finish meshing
            continue;
        };
        if early {
            // the early mesh can finish after the full one
            if !mesh.full {
                mesh.mesh = Some(chunk);
            }
            continue;
        }
        mesh.mesh = Some(chunk);
        mesh.full = true;
        *status = match *status {
            MeshStatus::InFlight { dirty: true } => MeshStatus::Stale,
            _ => MeshStatus::Meshed,
        };
    }
}

pub type ChunkReleaseQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut ChunkNeighbors,
        &'static ChunkPriority,
        &'static CountedState,
        &'static mut MissingNeighborCount,
    ),
>;

/// Detaches a chunk that is about to despawn from its neighbors: they forget it and no longer wait for it.
///
/// Without this a neighbor waiting for a chunk that never gets data would never be meshed.
pub fn release_chunk(chunk: Entity, query: &mut ChunkReleaseQuery) {
    let Ok((neighbors, _, counted, _)) = query.get(chunk) else {
        return;
    };
    let (neighbors, counted) = (*neighbors, *counted);
    for (idx, slot) in neighbors.0.iter().enumerate() {
        let Some(Some(ChunkEntity(neighbor))) = slot else {
            continue;
        };
        let relative_pos = ChunkNeighbors::relative_pos(idx);
        let Ok((mut neighbor_neighbors, neighbor_priority, neighbor_counted, mut count)) =
            query.get_mut(*neighbor)
        else {
            continue;
        };
        neighbor_neighbors.set(-relative_pos, None);
        // corners are not waited for. A neighbor with changed priority counts again, without this chunk.
        let is_mesh_neighbor = relative_pos.abs().element_sum() <= 2;
        if is_mesh_neighbor
            && neighbor_counted.priority == Some(*neighbor_priority)
            && blocks(counted.priority, counted.has_data, *neighbor_priority)
        {
            count.0 -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::manager::ChunkSubscriberPriority;
    use crate::chunk::neighbors::link_new_chunks;
    use crate::chunk::{Chunk, ChunkIndex, ChunkLod, AIR};
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::{HashMap, VecDeque};

    type Availability = [bool; 18];

    /// Records jobs; finishing them is up to the test so they can complete at any time
    #[derive(Default)]
    struct TestMesher {
        jobs: Vec<(Entity, Availability)>,
        finished: VecDeque<(Entity, Availability, bool)>,
        /// availability the currently installed full mesh of a chunk was made with
        installed: HashMap<Entity, Availability>,
    }

    impl ChunkMesherManager for TestMesher {
        fn schedule_meshing(
            &mut self,
            chunk_ref: ChunkReference,
            _: Arc<ChunkStorage>,
            neighbors: [Option<Arc<ChunkStorage>>; 18],
        ) {
            let availability = neighbors.map(|neighbor| neighbor.is_some());
            self.jobs.push((chunk_ref.entity.0, availability));
        }

        fn schedule_early_meshing(&mut self, _: ChunkReference, _: Arc<ChunkStorage>) {}

        fn pop(&mut self) -> Option<ChunkMesherOutput> {
            let (entity, availability, early) = self.finished.pop_front()?;
            if !early {
                self.installed.insert(entity, availability);
            }
            Some(ChunkMesherOutput {
                chunk: Arc::new(ChunkMesh(std::array::from_fn(|_| DirFaces {
                    standard: Vec::new(),
                    border: Vec::new(),
                }))),
                entity: ChunkEntity(entity),
                early,
            })
        }
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChunkIndex::new(1))
            .insert_resource(ChunkMesherResource(TestMesher::default()))
            .add_systems(PreUpdate, link_new_chunks)
            .add_plugins(ChunkMeshingPlugin::<TestMesher>::new());
        app
    }

    fn mesher(app: &mut App) -> &mut TestMesher {
        &mut app.world_mut().resource_mut::<ChunkMesherResource<TestMesher>>().into_inner().0
    }

    fn priority(batch: u8) -> ChunkPriority {
        ChunkPriority::new(ChunkSubscriberPriority::Normal, batch)
    }

    fn spawn(app: &mut App, position: IVec3, batch: u8) -> Entity {
        let entity = app
            .world_mut()
            .spawn((Chunk, ChunkPosition(position), ChunkLod(0), priority(batch)))
            .id();
        app.world_mut()
            .resource_mut::<ChunkIndex>()
            .get_mut(0)
            .insert(position, entity);
        entity
    }

    fn give_data(app: &mut App, entity: Entity) {
        app.world_mut().get_mut::<ChunkData>(entity).unwrap().0 =
            Some(Arc::new(ChunkStorage::new_filled(AIR)));
    }

    fn despawn(app: &mut App, entity: Entity) {
        let position = app.world().get::<ChunkPosition>(entity).unwrap().0;
        app.world_mut()
            .run_system_once(move |mut query: ChunkReleaseQuery| release_chunk(entity, &mut query))
            .unwrap();
        app.world_mut()
            .resource_mut::<ChunkIndex>()
            .get_mut(0)
            .remove(&position);
        app.world_mut().despawn(entity);
    }

    /// Finishes every scheduled job
    fn finish_jobs(app: &mut App) {
        let mesher = mesher(app);
        for (entity, availability) in std::mem::take(&mut mesher.jobs) {
            mesher.finished.push_back((entity, availability, false));
        }
    }

    fn status(app: &App, entity: Entity) -> MeshStatus {
        *app.world().get::<MeshStatus>(entity).unwrap()
    }

    fn scheduled(app: &mut App, entity: Entity) -> bool {
        mesher(app).jobs.iter().any(|&(job, _)| job == entity)
    }

    struct ChunkState {
        position: IVec3,
        priority: ChunkPriority,
        has_data: bool,
        neighbors: ChunkNeighbors,
        count: i32,
        status: MeshStatus,
    }

    /// Checks everything that should hold after an update against values calculated from scratch
    fn check_invariants(app: &mut App) {
        let world = app.world_mut();
        let index: HashMap<IVec3, Entity> = world
            .resource::<ChunkIndex>()
            .get(0)
            .iter()
            .map(|(&position, &entity)| (position, entity))
            .collect();
        let mut query = world.query::<(
            Entity,
            &ChunkPosition,
            &ChunkPriority,
            &ChunkData,
            &ChunkNeighbors,
            &MissingNeighborCount,
            &MeshStatus,
        )>();
        let states: HashMap<Entity, ChunkState> = query
            .iter(world)
            .map(|(entity, position, priority, data, neighbors, count, status)| {
                (
                    entity,
                    ChunkState {
                        position: position.0,
                        priority: *priority,
                        has_data: data.0.is_some(),
                        neighbors: *neighbors,
                        count: count.0,
                        status: *status,
                    },
                )
            })
            .collect();
        assert_eq!(states.len(), index.len());
        let mesher = &world.resource::<ChunkMesherResource<TestMesher>>().0;

        for (&entity, chunk) in &states {
            for idx in 0..26 {
                let relative_pos = ChunkNeighbors::relative_pos(idx);
                let expected = index.get(&(chunk.position + relative_pos)).copied();
                let slot = chunk.neighbors.0[idx].map(|slot| slot.map(|neighbor| neighbor.0));
                assert_eq!(slot, Some(expected), "{:?} slot {relative_pos}", chunk.position);
            }

            let mut expected_count = 0;
            let mut availability = [false; 18];
            for (i, &relative_pos) in CHUNK_MESH_NEIGHBORS.iter().enumerate() {
                let Some(neighbor) = index.get(&(chunk.position + relative_pos)) else {
                    continue;
                };
                let neighbor = &states[neighbor];
                availability[i] = neighbor.has_data;
                expected_count += blocks(Some(neighbor.priority), neighbor.has_data, chunk.priority) as i32;
            }
            assert_eq!(chunk.count, expected_count, "{:?} count", chunk.position);

            if chunk.has_data && chunk.count == 0 {
                assert!(
                    !matches!(chunk.status, MeshStatus::Unmeshed | MeshStatus::Stale),
                    "{:?} should have been scheduled",
                    chunk.position
                );
            }

            // a mesh made without data that is available now must be on its way to be replaced
            let made_with = match chunk.status {
                MeshStatus::Meshed => Some(mesher.installed[&entity]),
                MeshStatus::InFlight { dirty: false } => mesher
                    .jobs
                    .iter()
                    .map(|&(job, availability)| (job, availability))
                    .chain(mesher.finished.iter().map(|&(job, availability, _)| (job, availability)))
                    .rfind(|&(job, _)| job == entity)
                    .map(|(_, availability)| availability),
                _ => None,
            };
            if let Some(made_with) = made_with {
                for i in 0..18 {
                    assert!(
                        !availability[i] || made_with[i],
                        "{:?} {:?} lacks data of {}",
                        chunk.position,
                        chunk.status,
                        CHUNK_MESH_NEIGHBORS[i]
                    );
                }
            }
        }
    }

    #[test]
    fn mesh_neighbors_are_faces_and_edges() {
        let mut seen = std::collections::HashSet::new();
        for relative_pos in CHUNK_MESH_NEIGHBORS {
            assert!(seen.insert(relative_pos));
            assert!(relative_pos.abs().max_element() == 1);
            assert!(relative_pos.abs().element_sum() <= 2);
        }
    }

    #[test]
    fn neighbor_index_round_trips() {
        for idx in 0..26 {
            assert_eq!(ChunkNeighbors::neighbor_idx(ChunkNeighbors::relative_pos(idx)), idx);
        }
    }

    #[test]
    fn waits_for_equally_urgent_neighbors_only() {
        let mut app = test_app();
        let a = spawn(&mut app, IVec3::ZERO, 1);
        let urgent = spawn(&mut app, IVec3::X, 0);
        let same = spawn(&mut app, IVec3::Y, 1);
        let lazy = spawn(&mut app, IVec3::Z, 2);
        app.update();
        check_invariants(&mut app);
        assert_eq!(app.world().get::<MissingNeighborCount>(a).unwrap().0, 2);

        give_data(&mut app, a);
        give_data(&mut app, urgent);
        app.update();
        check_invariants(&mut app);
        assert!(!scheduled(&mut app, a), "still waits for the equally urgent neighbor");

        give_data(&mut app, same);
        app.update();
        check_invariants(&mut app);
        assert!(scheduled(&mut app, a), "does not wait for the less urgent neighbor");
        assert_eq!(status(&app, a), MeshStatus::InFlight { dirty: false });
        let _ = lazy;
    }

    #[test]
    fn remeshes_when_a_neighbor_arrives_later() {
        let mut app = test_app();
        let a = spawn(&mut app, IVec3::ZERO, 0);
        let lazy = spawn(&mut app, IVec3::X, 1);
        give_data(&mut app, a);
        app.update();
        assert_eq!(status(&app, a), MeshStatus::InFlight { dirty: false });

        // arrives while the first mesh is being made
        give_data(&mut app, lazy);
        app.update();
        assert_eq!(status(&app, a), MeshStatus::InFlight { dirty: true });
        check_invariants(&mut app);

        finish_jobs(&mut app);
        app.update();
        assert_eq!(status(&app, a), MeshStatus::InFlight { dirty: false }, "meshed again");
        check_invariants(&mut app);

        finish_jobs(&mut app);
        app.update();
        assert_eq!(status(&app, a), MeshStatus::Meshed);
        check_invariants(&mut app);
    }

    #[test]
    fn despawning_a_pending_neighbor_unblocks() {
        let mut app = test_app();
        let a = spawn(&mut app, IVec3::ZERO, 1);
        let pending = spawn(&mut app, IVec3::X, 1);
        give_data(&mut app, a);
        app.update();
        assert!(!scheduled(&mut app, a));

        despawn(&mut app, pending);
        app.update();
        check_invariants(&mut app);
        assert!(scheduled(&mut app, a));
    }

    #[test]
    fn early_mesh_does_not_replace_full_mesh() {
        let mut app = test_app();
        let a = spawn(&mut app, IVec3::ZERO, 0);
        give_data(&mut app, a);
        app.update();
        finish_jobs(&mut app);
        app.update();
        assert!(app.world().get::<ChunkMeshComponent>(a).unwrap().full);
        let full = app.world().get::<ChunkMeshComponent>(a).unwrap().mesh().unwrap().clone();

        mesher(&mut app).finished.push_back((a, [false; 18], true));
        app.update();
        let kept = app.world().get::<ChunkMeshComponent>(a).unwrap().mesh().unwrap().clone();
        assert!(Arc::ptr_eq(&full, &kept));
    }

    fn fuzz(seed: u64) {
        let mut rng = seed;
        let mut random = move |bound: usize| -> usize {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % bound as u64) as usize
        };
        let mut app = test_app();
        for frame in 0..300 {
            for _ in 0..1 + random(5) {
                let chunks: Vec<(Entity, bool, u8)> = {
                    let world = app.world_mut();
                    let mut query = world.query::<(Entity, &ChunkData, &ChunkPriority)>();
                    query
                        .iter(world)
                        .map(|(entity, data, priority)| (entity, data.0.is_some(), priority.batch_priority()))
                        .collect()
                };
                match random(7) {
                    6 => {
                        // an edit: data replaced by other data
                        let loaded: Vec<_> = chunks.iter().filter(|chunk| chunk.1).collect();
                        if !loaded.is_empty() {
                            give_data(&mut app, loaded[random(loaded.len())].0);
                        }
                    }
                    0 | 1 => {
                        let position =
                            IVec3::new(random(4) as i32, random(4) as i32, random(3) as i32);
                        let occupied = app.world().resource::<ChunkIndex>().get(0).contains_key(&position);
                        if !occupied {
                            spawn(&mut app, position, random(4) as u8);
                        }
                    }
                    2 | 3 => {
                        let pending: Vec<_> = chunks.iter().filter(|chunk| !chunk.1).collect();
                        if !pending.is_empty() {
                            give_data(&mut app, pending[random(pending.len())].0);
                        }
                    }
                    4 => {
                        if !chunks.is_empty() {
                            despawn(&mut app, chunks[random(chunks.len())].0);
                        }
                    }
                    _ => {
                        if !chunks.is_empty() {
                            let (entity, _, batch) = chunks[random(chunks.len())];
                            if batch > 0 {
                                let upgraded = priority(random(batch as usize) as u8);
                                app.world_mut()
                                    .get_mut::<ChunkPriority>(entity)
                                    .unwrap()
                                    .set_if_neq(upgraded);
                            }
                        }
                    }
                }
            }
            if random(2) == 0 {
                finish_jobs(&mut app);
            }
            app.update();
            check_invariants(&mut app);
            let _ = frame;
        }
    }

    #[test]
    fn counts_and_links_hold_under_random_changes() {
        for seed in 1..=40u64 {
            fuzz(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        }
    }
}
