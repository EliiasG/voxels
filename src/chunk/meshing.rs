use bevy::prelude::Component;
use bytemuck::{Pod, Zeroable};

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

#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct ChunkFaces(pub [DirFaces; 6]);

pub trait ChunkMesher {
    
}