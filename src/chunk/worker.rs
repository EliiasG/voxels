use std::sync::Arc;
use bevy::math::IVec3;
use crossbeam_channel::{Receiver, Sender};
use crate::chunk::{Chunk, ChunkData, ChunkEntity, ChunkStorage};
use crate::chunk::manager::{ChunkLoaderOutput, ChunkReference};
use crate::chunk::meshing::ChunkMesherOutput;
use crate::utils::{PrioritySender};

struct ChunkMesherInput {
    chunk: Arc<ChunkStorage>,
    neighbours: [Option<Arc<ChunkStorage>>; 18],
    entity: ChunkEntity,
}

struct ChunkMesherEarlyInput {
    chunk: Arc<ChunkStorage>,
    entity: ChunkEntity,
}

struct ChunkLoaderInput {
    position: IVec3,
    lod: usize,
    entity: ChunkEntity,
}

pub struct ChunkWorkerManager {
    chunk_load_sender: PrioritySender<ChunkLoaderInput>,
    chunk_mesh_sender: Sender<ChunkMesherInput>,
    chunk_mesh_early_sender: Sender<ChunkMesherEarlyInput>,
    chunk_receiver: Receiver<ChunkLoaderOutput>,
    chunk_mesh_receiver: Receiver<ChunkMesherOutput>
}

pub trait ChunkMesher {
    // TODO simple trait to handle chunk meshing, both with regular meshing and early meshing
}

pub trait ChunkGenerator {
    // TODO simple chunk gen that outputs chunkdata given position. Later on we will have a more interconnected system for shared calculation of nearby chunks.
}

//TODO ChunkWorkerManager should impl both ChunkLoader and ChunkMeshManger
// it should have a new function that takes num workers as arg and mesher / generator
// Each worker should have a loop that always tries to mesh first, and only generate if no mesh job available
// Should use traits defined above
// To control the loop we could have a simple channel (or perhaps just number or even a bool) that tells the workers if work is available

struct Worker {
    
}
