use crate::chunk::manager::{ChunkPriority, ChunkReference};
use crate::chunk::{ChunkData, ChunkEntity, ChunkIndex, ChunkPosition, ChunkStorage};
use bevy::math::IVec3;
use bevy::prelude::{Changed, Component, Query, Res};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

const CHUNK_MESH_NEIGHBORS: [IVec3; 18] = todo!();

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct FaceData {
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub w: u8,
    pub h: u8,
    pub material: [u8; 3],
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
}

pub trait ChunkMesher {
    // All possible neighbors except the 8 corners, needed for AO
    fn schedule_meshing(
        chunk_ref: ChunkReference,
        chunk_data: Arc<ChunkData>,
        neighbors: [Option<Arc<ChunkData>>; 18],
    );

    fn pop() -> Option<ChunkMesherOutput>;
}

/// Tracks how many chunk neighbors [CHUNK_MESH_NEIGHBORS] of same or less value (more urgent) are spawned but w/o data.
///
#[derive(Component, Clone, Copy, Default)]
pub struct MissingNeighborCount(usize);


// FIXME use direct neighbour reference instead of chunk_index
fn propagate_neighbor_count(
    chunk_index: Res<ChunkIndex>,
    // using changed as priorities might upgrade
    new_chunks: Query<(&ChunkPosition, &ChunkPriority, Option<&OldPriority>), Changed<ChunkPriority>>,
    mut old_chunks: Query<(&mut MissingNeighborCount, &ChunkPosition, &ChunkPriority)>,
) {
    for (pos, priority, old_priority) in old_chunks.iter_mut() {

    }
}
