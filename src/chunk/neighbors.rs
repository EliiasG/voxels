use crate::chunk::{Chunk, ChunkEntity, ChunkIndex, ChunkLod, ChunkNeighbors, ChunkPosition};
use bevy::prelude::*;

/// Fills [ChunkNeighbors] of chunks spawned by the chunk manager and writes them into the
/// neighbors they have.
///
/// Runs in `PreUpdate`: the manager spawns in `PostUpdate`, so from here until the manager
/// runs again every chunk is linked and systems can rely on [ChunkNeighbors] without ordering.
///
/// For each of the 26 locations the slot is, in order of preference:
/// 1. already filled, by a new neighbor linked earlier in this run
/// 2. inferred from a neighbor already found: its neighbor in the direction from it to the
///    location is the same location (this also yields "seen and empty" for free)
/// 3. looked up in the [ChunkIndex]
///
/// Inference can read a stale "empty" for a location where a chunk spawned this frame but is
/// not linked yet. That is fine: when that chunk links it finds this one (already linked, so
/// never stale) and overwrites the slot.
pub fn link_new_chunks(
    chunk_index: Res<ChunkIndex>,
    new_chunks: Query<(Entity, &ChunkPosition, &ChunkLod), Added<Chunk>>,
    mut neighbors: Query<&mut ChunkNeighbors>,
) {
    // (relative position, chunk) for neighbors found so far; kept between chunks to reuse the allocation
    let mut found: Vec<(IVec3, Entity)> = Vec::with_capacity(26);
    for (entity, &ChunkPosition(position), &ChunkLod(lod)) in &new_chunks {
        let Ok(mut slots) = neighbors.get(entity).copied() else {
            continue;
        };
        let map = chunk_index.get(lod);

        found.clear();
        for (idx, slot) in slots.0.iter().enumerate() {
            if let Some(Some(neighbor)) = slot {
                found.push((ChunkNeighbors::relative_pos(idx), neighbor.0));
            }
        }

        for idx in 0..26 {
            if slots.0[idx].is_some() {
                continue;
            }
            let relative_pos = ChunkNeighbors::relative_pos(idx);
            let inferred = found.iter().find_map(|&(found_pos, found_entity)| {
                let from_found = relative_pos - found_pos;
                if from_found == IVec3::ZERO || from_found.abs().max_element() > 1 {
                    return None;
                }
                neighbors.get(found_entity).ok()?.known(from_found)
            });
            let slot = inferred
                .unwrap_or_else(|| map.get(&(position + relative_pos)).copied().map(ChunkEntity));
            slots.0[idx] = Some(slot);
            if let Some(neighbor) = slot {
                found.push((relative_pos, neighbor.0));
            }
        }

        for &(relative_pos, neighbor) in &found {
            let Ok(mut other) = neighbors.get_mut(neighbor) else {
                eprintln!("chunk in index has no ChunkNeighbors");
                continue;
            };
            other.set(-relative_pos, Some(ChunkEntity(entity)));
        }
        if let Ok(mut mine) = neighbors.get_mut(entity) {
            *mine = slots;
        }
    }
}
