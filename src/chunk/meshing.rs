use crate::chunk::manager::{ChunkPriority, ChunkReference};
use crate::chunk::{Chunk, ChunkData, ChunkEntity, ChunkIndex, ChunkPosition, ChunkStorage};
use bevy::math::IVec3;
use bevy::prelude::{Changed, Component, Entity, Query, Res, With};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

/// Relative positions of neighbors needed for [ChunkMesherManager::schedule_meshing]
const CHUNK_MESH_NEIGHBORS: [IVec3; 18] = todo!();

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
pub struct ChunkMeshComponent(Option<Arc<ChunkMesh>>);

pub struct ChunkMesherOutput {
    pub chunk: Arc<ChunkMesh>,
    /// Corresponding chunk entity for mesh
    pub entity: ChunkEntity,
    /// If it was generated form the early function
    pub early: bool,
}

//TODO make a resource with one of these like for the chunkloader
pub trait ChunkMesherManager {
    // All possible neighbors except the 8 corners, needed for AO
    fn schedule_meshing(
        chunk_ref: ChunkReference,
        chunk_data: Arc<ChunkData>,
        neighbors: [Option<Arc<ChunkData>>; 18],
    );

    // To get a rough mesh quickly
    fn schedule_early_meshing(
        chunk_ref: ChunkReference,
        chunk_data: Arc<ChunkData>,
    );

    fn pop() -> Option<ChunkMesherOutput>;
}

/// Tracks how many chunk neighbors [CHUNK_MESH_NEIGHBORS] of same or less value (more urgent) are spawned but w/o data.
///
#[derive(Component, Clone, Copy, Default)]
pub struct MissingNeighborCount(usize);

#[derive(Component, Clone, Copy, Default)]
pub struct OldPriority(Option<ChunkPriority>);

fn schedule_early_meshes(
    new_chunks: Query<(Entity, &ChunkMeshComponent), Changed<ChunkMeshComponent>>,
) {todo!("call the chunkmesher schedule_early_meshing for newly genned chunks")}

//TODO system that pulls meshes from the mesher and adds mesh comps to chunks. I dont think it needs to care about early flag
fn pull_meshes() {todo!()}

// TODO use direct neighbour reference instead of chunk_index
fn decrement_neighbour_counts(
    chunk_index: Res<ChunkIndex>,
    // FIXME might cause problems if a chunk changes. currently i only think a chunk changes when its generated, but might need a proper fix
    mut new_chunks: Query<&ChunkPosition, Changed<ChunkData>>,
    mut neighbor_query: Query<&mut MissingNeighborCount>,
) {
    //TODO for all newly generated chunks, dec missing count on neighbours if priorities are correct (described elsewhere i think)
    // if neighbor count hits 0 add to the chunkmehser.
    todo!()
}

// TODO use direct neighbour reference instead of chunk_index
fn propagate_neighbor_count(
    chunk_index: Res<ChunkIndex>,
    // using changed as priorities might upgrade
    new_chunks: Query<(&ChunkPosition, &ChunkPriority, &OldPriority), Changed<ChunkPriority>>,
    mut old_chunks: Query<(&mut MissingNeighborCount, &ChunkPosition, &ChunkPriority)>,
) {
    for (pos, priority, OldPriority(old_priority)) in new_chunks {
        //TODO for all neighbors that are >= priority and < old_priority (if it exists), inc missing count. Reset own Missing Count. and have a later system calculate the own missing count. I think this can just be done with the same query
    }
    todo!()
}
