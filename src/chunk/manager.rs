use crate::chunk::{ChunkData, ChunkEntity, ChunkIndex, ChunkPosition, ChunkStorage};
use bevy::prelude::*;
use std::default::Default;
use std::marker::PhantomData;
use std::sync::Arc;

const MAX_CHUNKS_POPPED_PER_TICK: usize = 2048;

#[derive(Copy, Clone)]
pub struct SubscriberEntity(pub Entity);

pub struct ChunkManagerPlugin<G: ChunkLoader> {
    pd: PhantomData<G>,
}

impl<G: ChunkLoader> ChunkManagerPlugin<G> {
    pub fn new() -> Self {
        Self { pd: PhantomData }
    }
}

impl<G: ChunkLoader + Send + Sync + 'static> Plugin for ChunkManagerPlugin<G> {
    fn build(&self, app: &mut App) {
        app.add_message::<ChunkSubscribeMessage>()
            .add_message::<ChunkUnsubscribeMessage>()
            .add_systems(
                FixedUpdate,
                (
                    register_subscribers::<G>,
                    deregister_subscribers::<G>,
                    pop_chunks::<G>,
                    schedule_generation::<G>,
                    handle_unsubscribed_chunks::<G>,
                )
                    .chain(),
            );
    }
}

#[derive(Component, Default)]
pub struct ChunkSubscriber {
    priority: u32,
}

impl ChunkSubscriber {
    pub fn new(priority: u32) -> Self {
        Self { priority }
    }

    pub fn priority(&self) -> u32 {
        self.priority
    }
}

#[derive(Message)]
pub struct ChunkSubscribeMessage {
    pub subscriber: SubscriberEntity,
    pub buckets: Vec<ChunkList>,
}

#[derive(Message)]
pub struct ChunkUnsubscribeMessage {
    pub buckets: Vec<ChunkList>,
}

pub struct ChunkList {
    pub lod: usize,
    pub chunks: Vec<IVec3>,
}

pub struct ChunkReferenceList {
    pub lod: usize,
    pub chunks: Vec<ChunkReference>,
}

pub struct ChunkReference {
    pub position: IVec3,
    pub entity: ChunkEntity,
}

pub struct ChunkGeneratorOutput {
    pub chunk: Arc<ChunkStorage>,
    /// Corresponding chunk entity from subscribe message
    pub entity: ChunkEntity,
}

pub trait ChunkLoader {
    fn register_subscriber(&mut self, subscriber: SubscriberEntity, priority: u32);
    fn deregister_subscriber(&mut self, subscriber: SubscriberEntity);
    /// Register unloaded chunks from a [ChunkSubscribeMessage]. Might contain chunks that have already been registered.
    /// Entity ids should be kept for when outputting.
    fn register_subscribe_message(
        &mut self,
        subscriber: SubscriberEntity,
        buckets: &Vec<ChunkReferenceList>,
    );

    /// Called for chunks that are no longer needed
    fn free_chunks(&mut self, chunks: &ChunkList);

    fn pop(&mut self) -> Option<ChunkGeneratorOutput>;
}

#[derive(Resource)]
pub struct ChunkGeneratorResource<G: ChunkLoader>(pub G);

#[derive(Component)]
pub struct ChunkSubscriberCount(u32);

impl ChunkSubscriberCount {
    pub fn get(&self) -> u32 {
        self.0
    }
}

fn register_subscribers<G: ChunkLoader + Send + Sync + 'static>(
    mut generator: ResMut<ChunkGeneratorResource<G>>,
    subscribers: Query<(Entity, &ChunkSubscriber), Added<ChunkSubscriber>>,
) {
    for (subscriber, &ChunkSubscriber { priority }) in subscribers.iter() {
        generator
            .0
            .register_subscriber(SubscriberEntity(subscriber), priority);
    }
}

fn deregister_subscribers<G: ChunkLoader + Send + Sync + 'static>(
    mut generator: ResMut<ChunkGeneratorResource<G>>,
    mut subscribers: RemovedComponents<ChunkSubscriber>,
) {
    for entity in subscribers.read() {
        generator.0.deregister_subscriber(SubscriberEntity(entity));
    }
}

fn pop_chunks<G: ChunkLoader + Send + Sync + 'static>(
    mut commands: Commands,
    mut generator: ResMut<ChunkGeneratorResource<G>>,
) {
    for _ in 0..MAX_CHUNKS_POPPED_PER_TICK {
        // Test the budget before popping: popping past it would consume a finished
        // chunk from the generator and then drop it, stranding the entity in Loading.
        let Some(ChunkGeneratorOutput { chunk, entity }) = generator.0.pop() else {
            break;
        };
        let Ok(mut ec) = commands.get_entity(entity.0) else {
            // chunk might unload, but finish generating
            continue;
        };
        ec.insert(ChunkData(chunk));
    }
}

fn handle_unsubscribed_chunks<G: ChunkLoader + Send + Sync + 'static>(
    mut commands: Commands,
    mut reader: MessageReader<ChunkUnsubscribeMessage>,
    mut generator: ResMut<ChunkGeneratorResource<G>>,
    mut chunk_index: ResMut<ChunkIndex>,
    mut chunk_query: Query<&mut ChunkSubscriberCount>,
) {
    for message in reader.read() {
        for ChunkList { lod, chunks } in &message.buckets {
            let map = chunk_index.get_mut(*lod);
            let despawned = chunks
                .iter()
                .filter(|&position| {
                    //might as well have generator free on error (return true)
                    let Some(&chunk) = map.get(position) else {
                        eprintln!(
                            "tried to unload a chunk that is not loaded. Might result in leak"
                        );
                        return true;
                    };
                    let Ok(mut sub_count) = chunk_query.get_mut(chunk) else {
                        eprintln!("no subscriber count on chunk");
                        // stale entry: entity is gone but the index still points at it
                        map.remove(position);
                        return true;
                    };
                    if (sub_count.0 == 0) {
                        eprintln!("subscriber count == 0, cannot dec");
                        // probably already freed
                        return false;
                    }
                    sub_count.0 -= 1;
                    let despawn = sub_count.0 == 0;
                    if despawn {
                        commands.entity(chunk).despawn();
                        // drop the index entry too, or the coord points at a dead
                        // entity forever and can never be reloaded
                        map.remove(position);
                    }
                    despawn
                })
                .copied()
                .collect();
            generator.0.free_chunks(&ChunkList {
                lod: *lod,
                chunks: despawned,
            });
        }
    }
}

/// quite a big system; creates chunk entities and calls the generator
fn schedule_generation<G: ChunkLoader + Send + Sync + 'static>(
    mut commands: Commands,
    mut reader: MessageReader<ChunkSubscribeMessage>,
    mut generator: ResMut<ChunkGeneratorResource<G>>,
    mut chunk_index: ResMut<ChunkIndex>,
    mut chunk_query: Query<(Has<ChunkData>, &mut ChunkSubscriberCount)>,
) {
    for message in reader.read() {
        let buckets = message
            .buckets
            .iter()
            .map(|bucket| {
                let lod = bucket.lod;
                let map = chunk_index.get_mut(lod);
                let chunks = bucket
                    .chunks
                    .iter()
                    .filter_map(|&position| {
                        if let Some(&entity) = map.get(&position) {
                            // There is already a chunk, inc RC and scedule only if not generated
                            let Ok((is_loaded, mut sub_count)) = chunk_query.get_mut(entity) else {
                                eprintln!("Chunk in map, but has no ChunkSubscriberCount");
                                return None;
                            };
                            sub_count.0 += 1;
                            (!is_loaded).then(|| ChunkReference {
                                position,
                                entity: ChunkEntity(entity),
                            })
                        } else {
                            // no chunk entity, create one
                            let new = commands
                                .spawn((ChunkSubscriberCount(1), ChunkPosition(position)))
                                .id();
                            map.insert(position, new);
                            Some(ChunkReference {
                                position,
                                entity: ChunkEntity(new),
                            })
                        }
                    })
                    .collect();
                ChunkReferenceList { lod, chunks }
            })
            .collect();

        generator
            .0
            .register_subscribe_message(message.subscriber, &buckets);
    }
}
