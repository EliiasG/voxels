use crate::chunk::manager::{ChunkLoaderOutput, ChunkPositionBatch, ChunkLoader, ChunkReferenceBatch, SubscriberEntity, ChunkBatchPriority, ChunkSubscriberPriority};

pub struct SimpleChunkLoader {
    
}

impl ChunkLoader for SimpleChunkLoader {
    fn register_subscriber(&mut self, subscriber: SubscriberEntity, priority: ChunkSubscriberPriority) {
        todo!()
    }

    fn deregister_subscriber(&mut self, subscriber: SubscriberEntity) {
        todo!()
    }

    fn register_subscribe_message(&mut self, subscriber: SubscriberEntity, buckets: &Vec<(ChunkReferenceBatch, ChunkBatchPriority)>) {
        todo!()
    }

    fn free_chunks(&mut self, chunks: &ChunkPositionBatch) {
        todo!()
    }

    fn pop(&mut self) -> Option<ChunkLoaderOutput> {
        todo!()
    }
}