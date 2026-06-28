use crate::chunk::manager::{ChunkGeneratorOutput, ChunkList, ChunkLoader, ChunkReferenceList, SubscriberEntity};

pub struct SimpleChunkLoader {
    
}

impl ChunkLoader for SimpleChunkLoader {
    fn register_subscriber(&mut self, subscriber: SubscriberEntity, priority: u32) {
        todo!()
    }

    fn deregister_subscriber(&mut self, subscriber: SubscriberEntity) {
        todo!()
    }

    fn register_subscribe_message(&mut self, subscriber: SubscriberEntity, buckets: &Vec<ChunkReferenceList>) {
        todo!()
    }

    fn free_chunks(&mut self, chunks: &ChunkList) {
        todo!()
    }

    fn pop(&mut self) -> Option<ChunkGeneratorOutput> {
        todo!()
    }
}