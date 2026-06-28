use std::sync::Arc;
use bevy::math::IVec3;
use crate::chunk::{ChunkEntity, ChunkStorage, NUM_DIRECTIONS};
use crate::chunk::manager::ChunkReference;

pub mod simple_loader;

struct ChunkMesherInput {
    chunk: Arc<ChunkStorage>,
    neighbours: [Option<Arc<ChunkStorage>>; NUM_DIRECTIONS],
}

struct ChunkGeneratorInput {
    position: IVec3,
    lod: usize,
    entity: ChunkEntity,
}

enum WorkerInput {
    Generation(ChunkGeneratorInput),
    Mesher(ChunkMesherInput),
}

struct Worker {
    
}
