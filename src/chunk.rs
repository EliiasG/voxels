pub mod manager;
pub mod subscriber;
pub mod worker;
mod meshing;


use crate::chunk::manager::ChunkPriority;
use crate::chunk::meshing::ChunkMeshComponent;
use crate::chunk::meshing::MissingNeighborCount;
use crate::utils::{HashMap, PaletteVec};
use bevy::prelude::*;
use std::sync::Arc;

pub const CHUNK_SIZE: usize = 32;
pub const CHUNK_SIZE_2: usize = CHUNK_SIZE * CHUNK_SIZE;
pub const CHUNK_SIZE_3: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

pub const MAX_LOD_COUNT: usize = 16;

pub type BlockId = u32;
pub const AIR: BlockId = 0;
pub const STONE: BlockId = 1;
pub const DIRT: BlockId = 2;
pub const GRASS: BlockId = 3;
pub const GLASS: BlockId = 4;
pub const WATER: BlockId = 5;
pub const LAVA: BlockId = 6;
pub const TORCH: BlockId = 7;

#[derive(Clone)]
pub enum ChunkStorage {
    Filled(BlockId),
    Paletted { data: PaletteVec<BlockId> },
}

impl ChunkStorage {
    pub fn new_filled(block: BlockId) -> Self {
        ChunkStorage::Filled(block)
    }

    pub fn new_paletted(data: PaletteVec<BlockId>) -> Self {
        assert_eq!(data.len(), CHUNK_SIZE_3);
        ChunkStorage::Paletted { data }
    }

    pub fn from_flat_array(data: Vec<BlockId>) -> Self {
        assert_eq!(data.len(), CHUNK_SIZE_3);
        let pv = PaletteVec::new(data);
        if pv.palette_len() == 1 {
            ChunkStorage::Filled(*pv.get(0))
        } else {
            ChunkStorage::Paletted { data: pv }
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockId {
        let index = x + y * CHUNK_SIZE + z * CHUNK_SIZE_2;
        match self {
            ChunkStorage::Filled(block) => *block,
            ChunkStorage::Paletted { data } => *data.get(index),
        }
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, value: BlockId) {
        let index = x + y * CHUNK_SIZE + z * CHUNK_SIZE_2;
        match self {
            ChunkStorage::Filled(block) => {
                if *block == value {
                    return; // still uniform, nothing to do
                }
                // promote: a paletted chunk filled with the old block, then apply the edit
                let mut data = PaletteVec::filled(CHUNK_SIZE_3, *block);
                data.set(index, &value);
                *self = ChunkStorage::Paletted { data };
            }
            ChunkStorage::Paletted { data } => {
                data.set(index, &value);
            }
        }
    }

    pub fn compress(&mut self) {
        let ChunkStorage::Paletted { data } = self else {
            return;
        };
        if let Some(block) = data.single_value() {
            *self = ChunkStorage::Filled(*block);
            return;
        }
        data.compress();
    }
}

#[derive(Component)]
#[require(
    ChunkData,
    ChunkPosition,
    ChunkNeighbors,
    ChunkMeshComponent,
    ChunkPriority,
    MissingNeighborCount
)]
struct Chunk;

#[derive(Copy, Clone)]
pub struct ChunkEntity(pub Entity);

#[derive(Component, Default)]
pub struct ChunkNeighbors(pub [Option<ChunkEntity>; 26]);

impl ChunkNeighbors {
    pub fn neighbor_idx(mut relative_pos: IVec3) -> usize {
        // must be max abs 1 and not 0
        debug_assert!(relative_pos.abs().max_element() == 1);
        relative_pos += IVec3::ONE;
        let mut idx = relative_pos.x + relative_pos.y * 3 + relative_pos.z * 9;
        if idx > 1 + 3 + 9 {
            idx -= 1;
        }
        idx as usize
    }

    pub fn relative_pos(mut neighbor_idx: usize) -> IVec3 {
        debug_assert!(neighbor_idx < 26);
        if neighbor_idx >= 1 + 3 + 9 {
            neighbor_idx += 1;
        }
        IVec3::new(
            (neighbor_idx % 3) as i32,
            (neighbor_idx / 3 % 3) as i32,
            (neighbor_idx / 9) as i32,
        ) - IVec3::ONE
    }

    pub fn neighbor(&self, relative_pos: IVec3) -> Option<ChunkEntity> {
        self.0[Self::neighbor_idx(relative_pos)]
    }
}

#[derive(Component, Clone, Default)]
pub struct ChunkData(pub Option<Arc<ChunkStorage>>);

#[derive(Component, Clone, Default)]
pub struct ChunkPosition(pub IVec3);

//TODO use 64 bit keys (22bit X/Z, 20bit Y)
pub type ChunkHashMap = HashMap<IVec3, Entity>;

//TODO possible dimensions?
#[derive(Resource)]
pub struct ChunkIndex {
    maps: [ChunkHashMap; MAX_LOD_COUNT],
    lod_count: usize,
}

impl ChunkIndex {
    pub fn new(lod_count: usize) -> Self {
        assert!(lod_count < MAX_LOD_COUNT);
        Self {
            maps: std::array::from_fn(|_| Default::default()),
            lod_count,
        }
    }

    #[inline]
    pub fn get(&self, lod: usize) -> &ChunkHashMap {
        debug_assert!(lod < self.lod_count);
        &self.maps[lod]
    }

    #[inline]
    pub fn get_mut(&mut self, lod: usize) -> &mut ChunkHashMap {
        debug_assert!(lod < self.lod_count);
        &mut self.maps[lod]
    }
}