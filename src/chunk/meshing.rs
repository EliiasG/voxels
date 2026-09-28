use crate::chunk::manager::ChunkReference;
use crate::chunk::{ChunkData, ChunkEntity, ChunkStorage};
use bevy::prelude::Component;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

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

#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct ChunkMeshComponent(Arc<ChunkMesh>);

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
