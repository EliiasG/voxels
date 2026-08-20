mod vec;
mod bitmask;
mod priority_channel;


pub use vec::{BoolIter, BoolVec, PackedIter, PackedVec, PaletteIter, PaletteVec};
pub use bitmask::{FixedBitMask, NestedFixedBitMask, BitMask4096, BitMask262144};
pub use priority_channel::{
    channel as priority_channel, AtomicBitset4096, Consumer, Producer, PRIORITIES,
};



pub type HashMap<K, V> = bevy::platform::collections::HashMap<K, V>;
pub type HashSet<T> = bevy::platform::collections::HashSet<T>;