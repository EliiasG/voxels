mod vec;
mod bitmask;


pub use vec::{BoolIter, BoolVec, PackedIter, PackedVec, PaletteIter, PaletteVec};
pub use bitmask::{FixedBitMask, NestedFixedBitMask, BitMask4096, BitMask262144};



pub type HashMap<K, V> = bevy::platform::collections::HashMap<K, V>;
pub type HashSet<T> = bevy::platform::collections::HashSet<T>;