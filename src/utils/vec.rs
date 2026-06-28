/// Structure for efficiently storing small values, using only as many bits as needed for the max value.
#[derive(Clone, Debug)]
pub struct PackedVec {
    data: Vec<u64>,
    size: usize,
    bits_per_value: usize,
}

impl PackedVec {
    pub fn empty() -> Self {
        Self {
            data: vec![],
            size: 0,
            bits_per_value: 1,
        }
    }

    /// Calculates bits required for max value and creates a new [PackedVec].
    pub fn new(data: &[u64]) -> Self {
        let Some(&mx) = data.iter().max() else {
            return Self::empty();
        };
        Self::new_with_bits(data, Self::required_bits(mx))
    }

    /// Creates a new [PackedVec] with the specified bits per value. All values in the input data must fit within the specified bits
    pub fn new_with_bits(data: &[u64], bits_per_value: usize) -> Self {
        let values_per_word = 64 / bits_per_value;
        let required_words = data.len().div_ceil(values_per_word);
        let packed = (0..required_words)
            .map(|i| {
                let start = i * values_per_word;
                let end = ((i + 1) * values_per_word).min(data.len());
                let mut word = 0u64;
                for j in start..end {
                    word |= data[j] << ((j - start) * bits_per_value);
                }
                word
            })
            .collect();
        Self {
            data: packed,
            size: data.len(),
            bits_per_value,
        }
    }

    /// Creates a vec of `size` copies of `value`, with the index width sized to hold
    /// `value`. Builds the words directly (O(size / values_per_word)), so it is much
    /// cheaper than an empty vec followed by a `value`-fill resize.
    pub fn filled(size: usize, value: u64) -> Self {
        let bits_per_value = Self::required_bits(value);
        let values_per_word = 64 / bits_per_value;
        let words = size.div_ceil(values_per_word);
        let masked = value & ((1u64 << bits_per_value) - 1);
        let mut word = 0u64;
        for slot in 0..values_per_word {
            word |= masked << (slot * bits_per_value);
        }
        let mut data = vec![word; words];
        if size > 0 {
            // clear the phantom tail of the last word so the buffer stays canonical
            let used_bits = (size - (words - 1) * values_per_word) * bits_per_value;
            if used_bits < 64 {
                data[words - 1] &= (1u64 << used_bits) - 1;
            }
        }
        Self {
            data,
            size,
            bits_per_value,
        }
    }

    /// Number of values stored.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Returns `true` if no values are stored.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Bits used to store each value.
    pub fn bits_per_value(&self) -> usize {
        self.bits_per_value
    }

    /// Re-packs every value to `bits_per_value` bits each, in place.
    /// Lowering will discard bits above bits_per_value.
    pub fn set_bits_per_value(&mut self, bits_per_value: usize) {
        debug_assert!((1..=64).contains(&bits_per_value));
        let old = self.bits_per_value;
        if bits_per_value == old {
            return;
        }
        let new_words = self.size.div_ceil(64 / bits_per_value);

        if bits_per_value > old {
            // Growing: every value's new slot sits at or above its old slot. Grow the
            // buffer first, then walk back-to-front. Each value is read into a local
            // before it is written, and the write never reaches down into a not-yet-read
            // slot, so old data is never clobbered.
            self.data.resize(new_words, 0);
            for i in (0..self.size).rev() {
                let v = Self::read_at(&self.data, i, old);
                Self::write_at(&mut self.data, i, bits_per_value, v);
            }
        } else {
            // Shrinking: every new slot sits at or below its old slot. Walk front-to-back,
            // then drop the now-unused tail words.
            for i in 0..self.size {
                let v = Self::read_at(&self.data, i, old);
                Self::write_at(&mut self.data, i, bits_per_value, v);
            }
            self.data.truncate(new_words);
        }

        // Zero the unused high bits of the last word so the raw buffer stays canonical
        // (matters only if you hash / compare / serialize `data` directly).
        if self.size > 0 {
            let used = self.size - (new_words - 1) * (64 / bits_per_value);
            let used_bits = used * bits_per_value;
            if used_bits < 64 {
                self.data[new_words - 1] &= (1u64 << used_bits) - 1;
            }
        }

        self.bits_per_value = bits_per_value;
    }

    /// Returns the value stored at `index`. Debug-asserts `index < size`; in release
    /// an out-of-range index is a logic error (may read a neighbouring slot or panic).
    #[inline]
    pub fn get(&self, index: usize) -> u64 {
        debug_assert!(
            index < self.size,
            "index {index} out of range (size {})",
            self.size
        );
        Self::read_at(&self.data, index, self.bits_per_value)
    }

    /// Stores `value` at `index`. Bits of `value` above `bits_per_value` are discarded.
    /// Debug-asserts `index < size` (out-of-range is a logic error in release).
    #[inline]
    pub fn set(&mut self, index: usize, value: u64) {
        debug_assert!(
            index < self.size,
            "index {index} out of range (size {})",
            self.size
        );
        Self::write_at(&mut self.data, index, self.bits_per_value, value);
    }

    /// Appends `value` to the end. Bits above `bits_per_value` are discarded
    /// (widen with `set_bits_per_value` first if the value needs a wider slot).
    pub fn push(&mut self, value: u64) {
        let values_per_word = 64 / self.bits_per_value;
        if self.size % values_per_word == 0 {
            self.data.push(0); // this value starts a fresh word
        }
        Self::write_at(&mut self.data, self.size, self.bits_per_value, value);
        self.size += 1;
    }

    /// Removes and returns the last value, or `None` if empty.
    pub fn pop(&mut self) -> Option<u64> {
        if self.size == 0 {
            return None;
        }
        self.size -= 1;
        let value = Self::read_at(&self.data, self.size, self.bits_per_value);
        let words = self.size.div_ceil(64 / self.bits_per_value);
        if self.data.len() > words {
            self.data.truncate(words); // last word no longer needed
        } else {
            Self::write_at(&mut self.data, self.size, self.bits_per_value, 0); // keep tail canonical
        }
        Some(value)
    }

    /// Resizes to `new_size`, filling any new slots with `value` (bits above
    /// `bits_per_value` discarded). Shrinking drops trailing values.
    pub fn resize(&mut self, new_size: usize, value: u64) {
        let values_per_word = 64 / self.bits_per_value;
        let new_words = new_size.div_ceil(values_per_word);
        if new_size > self.size {
            self.data.resize(new_words, 0);
            if value != 0 {
                // value == 0 is already covered by the zeroed words / clean phantom bits
                for i in self.size..new_size {
                    Self::write_at(&mut self.data, i, self.bits_per_value, value);
                }
            }
            self.size = new_size;
        } else if new_size < self.size {
            self.size = new_size;
            self.data.truncate(new_words);
            if new_size > 0 {
                let used_bits =
                    (new_size - (new_words - 1) * values_per_word) * self.bits_per_value;
                if used_bits < 64 {
                    self.data[new_words - 1] &= (1u64 << used_bits) - 1; // clear phantom tail
                }
            }
        }
    }

    /// Iterates the stored values in order. Walks words/slots directly, so there
    /// is no per-element division.
    #[inline]
    pub fn iter(&self) -> PackedIter<'_> {
        PackedIter {
            data: &self.data,
            bits_per_value: self.bits_per_value,
            mask: (1u64 << self.bits_per_value) - 1,
            word: 0,
            shift: 0,
            remaining: self.size,
        }
    }

    pub fn required_bits(max_value: u64) -> usize {
        let mut bits = 1;
        while 1 << bits <= max_value {
            bits += 1;
        }
        bits
    }

    #[inline]
    fn read_at(data: &[u64], index: usize, bits_per_value: usize) -> u64 {
        let values_per_word = 64 / bits_per_value;
        let shift = (index % values_per_word) * bits_per_value;
        let mask = (1u64 << bits_per_value) - 1;
        (data[index / values_per_word] >> shift) & mask
    }

    #[inline]
    fn write_at(data: &mut [u64], index: usize, bits_per_value: usize, value: u64) {
        let values_per_word = 64 / bits_per_value;
        let shift = (index % values_per_word) * bits_per_value;
        let mask = (1u64 << bits_per_value) - 1;
        let word = &mut data[index / values_per_word];
        *word = (*word & !(mask << shift)) | ((value & mask) << shift);
    }
}

/// Iterator over the values of a [`PackedVec`], yielding each as a `u64`.
pub struct PackedIter<'a> {
    data: &'a [u64],
    bits_per_value: usize,
    mask: u64,
    word: usize,
    shift: usize,
    remaining: usize,
}

impl Iterator for PackedIter<'_> {
    type Item = u64;

    #[inline]
    fn next(&mut self) -> Option<u64> {
        if self.remaining == 0 {
            return None;
        }
        let value = (self.data[self.word] >> self.shift) & self.mask;
        self.shift += self.bits_per_value;
        if self.shift + self.bits_per_value > 64 {
            // remaining bits in this word can't hold another value (aligned packing)
            self.shift = 0;
            self.word += 1;
        }
        self.remaining -= 1;
        Some(value)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for PackedIter<'_> {}

impl<'a> IntoIterator for &'a PackedVec {
    type Item = u64;
    type IntoIter = PackedIter<'a>;

    #[inline]
    fn into_iter(self) -> PackedIter<'a> {
        self.iter()
    }
}

/// A sequence of `T` values stored compactly: distinct values live once in a
/// palette, and each slot stores a small index into it (via [`PackedVec`]).
///
/// Raw palette indices (`usize`) are deliberately plain integers, not borrow- or
/// generation-checked handles. They are valid until the palette changes; the only
/// thing that reorders the palette is [`compress`](Self::compress), which is a
/// manual, deferrable operation — never call it while reusing a cached raw index.
#[derive(Clone, Debug)]
pub struct PaletteVec<T: Eq> {
    palette: Vec<T>,
    data: PackedVec,
}

impl<T: Eq> PaletteVec<T> {
    pub fn empty() -> Self {
        Self {
            palette: Vec::new(),
            data: PackedVec::empty(),
        }
    }

    /// Creates an empty (zero-length) vec that already knows `palette`, so its
    /// entries can be referenced before any value is stored.
    pub fn with_palette(palette: Vec<T>) -> Self {
        let bits = Self::bits_for_len(palette.len());
        Self {
            palette,
            data: PackedVec::new_with_bits(&[], bits),
        }
    }

    /// Builds a palette-compressed vec from a full sequence of values.
    pub fn new(values: Vec<T>) -> Self {
        let mut palette: Vec<T> = Vec::new();
        let mut indices: Vec<u64> = Vec::with_capacity(values.len());
        for value in values {
            let index = match palette.iter().position(|p| p == &value) {
                Some(i) => i,
                None => {
                    palette.push(value);
                    palette.len() - 1
                }
            };
            indices.push(index as u64);
        }
        Self {
            data: PackedVec::new(&indices),
            palette,
        }
    }

    /// Creates a vec of `size` copies of `value` — a single-entry palette with every
    /// slot pointing at it. This is the natural representation of a uniform region
    /// (e.g. promoting a single-block chunk), and [`single_value`](Self::single_value)
    /// reports it in O(1).
    pub fn filled(size: usize, value: T) -> Self {
        Self {
            palette: vec![value],
            data: PackedVec::filled(size, 0),
        }
    }

    /// Number of stored values.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Number of distinct values currently in the palette (includes dead entries
    /// until [`compress`](Self::compress) is called).
    pub fn palette_len(&self) -> usize {
        self.palette.len()
    }

    /// Bits used per stored index.
    pub fn bits_per_value(&self) -> usize {
        self.data.bits_per_value()
    }

    pub fn palette(&self) -> &[T] {
        &self.palette
    }

    /// Returns `true` if no values are stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the value shared by every slot, or `None` if the vec is empty or holds
    /// more than one distinct value.
    ///
    /// O(1) when the palette holds a single entry; otherwise one O(size) pass with
    /// early exit on the first mismatch — far cheaper than [`compress`](Self::compress),
    /// which makes three passes. Handy for collapsing a uniform chunk to a single block.
    pub fn single_value(&self) -> Option<&T> {
        if self.len() == 0 {
            return None;
        }
        let first = self.data.get(0);
        // A single-entry palette is uniform by construction (every raw index must be 0),
        // so we can answer without touching the data.
        if self.palette.len() <= 1 || self.data.iter().all(|raw| raw == first) {
            Some(self.palette_value(first as usize))
        } else {
            None
        }
    }

    /// Iterates the stored values in order, yielding a reference into the palette.
    pub fn iter(&self) -> PaletteIter<'_, T> {
        PaletteIter {
            inner: self.data.iter(),
            palette: &self.palette,
        }
    }

    // --- Safe, value-based layer --------------------------------------------

    /// Returns the value stored at `index`.
    #[inline]
    pub fn get(&self, index: usize) -> &T {
        self.palette_value(self.get_raw(index))
    }

    /// Stores `value` at `index`, inserting it into the palette if absent.
    /// O(palette_len) for the lookup; may widen the index storage on insert.
    #[inline]
    pub fn set(&mut self, index: usize, value: &T)
    where
        T: Clone,
    {
        let raw = self.get_or_insert_index(value);
        self.set_raw(index, raw);
    }

    /// Appends `value` to the end. O(palette_len).
    pub fn push(&mut self, value: &T)
    where
        T: Clone,
    {
        let raw = self.get_or_insert_index(value);
        self.data.push(raw as u64);
    }

    /// Removes and returns the last value, or `None` if empty. The palette is left
    /// untouched — a now-unreferenced entry survives until [`compress`](Self::compress).
    pub fn pop(&mut self) -> Option<T>
    where
        T: Clone,
    {
        let raw = self.data.pop()? as usize;
        Some(self.palette[raw].clone())
    }

    /// Resizes to `new_size`. Growing fills new slots with `value` (inserting it
    /// into the palette if absent); shrinking drops trailing slots without
    /// reclaiming any palette entries.
    pub fn resize(&mut self, new_size: usize, value: &T)
    where
        T: Clone,
    {
        if new_size > self.data.len() {
            let raw = self.get_or_insert_index(value);
            self.data.resize(new_size, raw as u64);
        } else {
            // Shrinking: PackedVec ignores the fill value, so don't insert one.
            self.data.resize(new_size, 0);
        }
    }

    // --- Raw index layer (fast path) ----------------------------------------

    /// Raw palette index of `value`, or `None` if it is not in the palette.
    /// O(palette_len).
    pub fn index_of(&self, value: &T) -> Option<usize> {
        self.palette.iter().position(|p| p == value)
    }

    /// Raw palette index of `value`, inserting it (and widening if the palette
    /// crosses a power-of-two boundary) if absent. O(palette_len).
    pub fn get_or_insert_index(&mut self, value: &T) -> usize
    where
        T: Clone,
    {
        if let Some(i) = self.index_of(value) {
            return i;
        }
        self.palette.push(value.clone());
        let needed = Self::bits_for_len(self.palette.len());
        if needed > self.data.bits_per_value() {
            self.data.set_bits_per_value(needed);
        }
        self.palette.len() - 1
    }

    /// The palette value a raw index points to.
    #[inline]
    pub fn palette_value(&self, raw: usize) -> &T {
        &self.palette[raw]
    }

    /// Raw palette index stored at `index`. O(1).
    #[inline]
    pub fn get_raw(&self, index: usize) -> usize {
        self.data.get(index) as usize
    }

    /// Writes a raw palette index at `index`. O(1). `raw` must already be a valid
    /// palette index (e.g. from [`get_or_insert_index`](Self::get_or_insert_index)).
    #[inline]
    pub fn set_raw(&mut self, index: usize, raw: usize) {
        debug_assert!(raw < self.palette.len(), "raw index {raw} not in palette");
        self.data.set(index, raw as u64);
    }

    // --- Maintenance --------------------------------------------------------

    /// Reclaims palette entries no longer referenced by any slot, remapping the
    /// stored indices and shrinking the index width if the live count allows.
    ///
    /// O(size) — does a full scan and rewrite of the data, so it is meant for
    /// idle/unload/save time, never the per-edit hot path.
    pub fn compress(&mut self) {
        let size = self.data.len();
        if self.palette.is_empty() {
            return;
        }

        // Which palette entries are still referenced?
        let mut used = vec![false; self.palette.len()];
        for i in 0..size {
            used[self.data.get(i) as usize] = true;
        }
        if used.iter().all(|&u| u) {
            return; // nothing dead -> palette already minimal
        }

        // Dense remap, preserving order so every new index <= its old index
        // (which keeps the in-place rewrite below within the current width).
        let mut remap = vec![0usize; self.palette.len()];
        let old_palette = std::mem::take(&mut self.palette);
        for (old, entry) in old_palette.into_iter().enumerate() {
            if used[old] {
                remap[old] = self.palette.len();
                self.palette.push(entry);
            }
        }

        for i in 0..size {
            let old = self.data.get(i) as usize;
            self.data.set(i, remap[old] as u64);
        }
        self.data.set_bits_per_value(Self::bits_for_len(self.palette.len()));
    }

    /// Bits needed to index a palette of `len` entries (minimum 1).
    fn bits_for_len(len: usize) -> usize {
        PackedVec::required_bits(len.saturating_sub(1) as u64)
    }
}

/// Iterator over the values of a [`PaletteVec`], yielding `&T` references into the palette.
pub struct PaletteIter<'a, T> {
    inner: PackedIter<'a>,
    palette: &'a [T],
}

impl<'a, T> Iterator for PaletteIter<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        self.inner.next().map(|raw| &self.palette[raw as usize])
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> ExactSizeIterator for PaletteIter<'_, T> {}

impl<'a, T: Eq> IntoIterator for &'a PaletteVec<T> {
    type Item = &'a T;
    type IntoIter = PaletteIter<'a, T>;

    #[inline]
    fn into_iter(self) -> PaletteIter<'a, T> {
        self.iter()
    }
}

/// A bit vector: one `bool` per bit, backed by a [`PackedVec`] fixed at 1 bit per value.
#[derive(Clone, Debug)]
pub struct BoolVec {
    data: PackedVec,
}

impl BoolVec {
    pub fn empty() -> Self {
        // PackedVec::empty() already uses 1 bit per value.
        Self {
            data: PackedVec::empty(),
        }
    }

    /// Creates a vec of `size` copies of `value`.
    pub fn filled(size: usize, value: bool) -> Self {
        Self {
            data: PackedVec::filled(size, value as u64),
        }
    }

    /// Builds a bit vector from a slice of bools.
    pub fn new(values: &[bool]) -> Self {
        let mut bits = Self::filled(values.len(), false);
        for (i, &b) in values.iter().enumerate() {
            if b {
                bits.data.set(i, 1);
            }
        }
        bits
    }

    /// Number of bits stored.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if no bits are stored.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the bit at `index`. Debug-asserts `index < len`.
    #[inline]
    pub fn get(&self, index: usize) -> bool {
        self.data.get(index) != 0
    }

    /// Sets the bit at `index`. Debug-asserts `index < len`.
    #[inline]
    pub fn set(&mut self, index: usize, value: bool) {
        self.data.set(index, value as u64);
    }

    /// Appends a bit to the end.
    pub fn push(&mut self, value: bool) {
        self.data.push(value as u64);
    }

    /// Removes and returns the last bit, or `None` if empty.
    pub fn pop(&mut self) -> Option<bool> {
        self.data.pop().map(|v| v != 0)
    }

    /// Resizes to `new_size`, filling any new bits with `value`.
    pub fn resize(&mut self, new_size: usize, value: bool) {
        self.data.resize(new_size, value as u64);
    }

    /// Number of bits set to `true`. O(len) — iterates the bits. (A word-level
    /// popcount would be ~64× faster but needs `PackedVec` to expose its raw words.)
    pub fn count_ones(&self) -> usize {
        self.iter().filter(|&b| b).count()
    }

    /// Iterates the stored bits in order.
    pub fn iter(&self) -> BoolIter<'_> {
        BoolIter {
            inner: self.data.iter(),
        }
    }
}

/// Iterator over the bits of a [`BoolVec`].
pub struct BoolIter<'a> {
    inner: PackedIter<'a>,
}

impl Iterator for BoolIter<'_> {
    type Item = bool;

    #[inline]
    fn next(&mut self) -> Option<bool> {
        self.inner.next().map(|v| v != 0)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for BoolIter<'_> {}

impl<'a> IntoIterator for &'a BoolVec {
    type Item = bool;
    type IntoIter = BoolIter<'a>;

    #[inline]
    fn into_iter(self) -> BoolIter<'a> {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words_for(size: usize, bpv: usize) -> usize {
        size.div_ceil(64 / bpv)
    }

    #[test]
    fn push_then_get_and_pop_lifo() {
        let mut pv = PackedVec::empty();
        pv.set_bits_per_value(3); // hold 0..7
        for v in 0..50u64 {
            pv.push(v % 8);
        }
        assert_eq!(pv.len(), 50);
        assert_eq!(pv.data.len(), words_for(50, 3)); // word invariant holds across boundaries
        for i in 0..50 {
            assert_eq!(pv.get(i), (i as u64) % 8);
        }
        for v in (0..50u64).rev() {
            assert_eq!(pv.pop(), Some(v % 8));
        }
        assert_eq!(pv.pop(), None);
        assert_eq!(pv.len(), 0);
        assert!(pv.data.is_empty());
    }

    #[test]
    fn resize_grows_with_fill_and_shrinks_clean() {
        let mut pv = PackedVec::empty();
        pv.set_bits_per_value(4);
        pv.resize(30, 9); // grow from empty, fill with 9
        assert_eq!(pv.len(), 30);
        assert!((0..30).all(|i| pv.get(i) == 9));

        pv.resize(10, 0); // shrink
        assert_eq!(pv.len(), 10);
        assert!((0..10).all(|i| pv.get(i) == 9));
        // phantom tail of last word must be zero after shrink
        let vpw = 64 / pv.bits_per_value();
        let used_bits = (pv.len() - (pv.data.len() - 1) * vpw) * pv.bits_per_value();
        if used_bits < 64 {
            assert_eq!(*pv.data.last().unwrap() >> used_bits, 0);
        }
    }

    #[test]
    fn push_after_repack_round_trips() {
        let mut pv = PackedVec::new(&[1, 2, 3]); // 2 bits
        pv.set_bits_per_value(5);
        pv.push(30); // fits in 5 bits
        assert_eq!(pv.len(), 4);
        assert_eq!([pv.get(0), pv.get(1), pv.get(2), pv.get(3)], [1, 2, 3, 30]);
    }

    #[test]
    fn palette_new_dedupes_and_round_trips() {
        let values = vec![10u32, 20, 10, 30, 20, 10];
        let pv = PaletteVec::new(values.clone());
        assert_eq!(pv.len(), 6);
        assert_eq!(pv.palette_len(), 3); // 10, 20, 30
        for (i, v) in values.iter().enumerate() {
            assert_eq!(pv.get(i), v);
        }
    }

    #[test]
    fn palette_set_inserts_and_widens() {
        let mut pv = PaletteVec::new(vec![0u32; 8]); // 1 entry, 1 bit
        assert_eq!(pv.bits_per_value(), 1);
        // introduce enough distinct values to force a wider index
        for i in 0..8 {
            pv.set(i, &(i as u32 * 100));
        }
        assert_eq!(pv.palette_len(), 8); // 0,100,200,...,700 (0 already present)
        assert_eq!(pv.bits_per_value(), 3); // 8 entries -> 3 bits
        for i in 0..8 {
            assert_eq!(*pv.get(i), i as u32 * 100);
        }
    }

    #[test]
    fn palette_compress_reclaims_and_shrinks() {
        // 20 distinct values -> palette 20, 5 bits.
        let mut pv = PaletteVec::new((0u32..20).collect());
        assert_eq!(pv.bits_per_value(), 5);
        // overwrite every slot with just two values (adds 2 dead-making entries).
        for i in 0..20 {
            pv.set(i, &(if i % 2 == 0 { 100 } else { 200 }));
        }
        assert!(pv.palette_len() >= 20);

        pv.compress();
        assert_eq!(pv.palette_len(), 2);
        assert_eq!(pv.bits_per_value(), 1); // 2 live entries -> 1 bit
        for i in 0..20 {
            assert_eq!(*pv.get(i), if i % 2 == 0 { 100 } else { 200 });
        }
    }

    #[test]
    fn palette_raw_fast_path() {
        let mut pv = PaletteVec::new(vec![5u32, 5, 5, 5]);
        let raw = pv.get_or_insert_index(&7); // resolve once
        for i in 0..4 {
            pv.set_raw(i, raw); // O(1) writes reusing the index
        }
        assert!((0..4).all(|i| *pv.get(i) == 7));
        assert_eq!(pv.index_of(&5), Some(0));
        assert_eq!(pv.index_of(&999), None);
    }

    #[test]
    fn palette_pop() {
        let mut pv = PaletteVec::new(vec![1u32, 2, 3]);
        assert_eq!(pv.pop(), Some(3));
        assert_eq!(pv.pop(), Some(2));
        assert_eq!(pv.len(), 1);
        assert_eq!(pv.pop(), Some(1));
        assert_eq!(pv.pop(), None);
        assert_eq!(pv.len(), 0);
    }

    #[test]
    fn palette_resize_grows_and_shrinks() {
        let mut pv = PaletteVec::new(vec![7u32, 7]);
        pv.resize(5, &9); // grow, fill new slots with 9
        assert_eq!(pv.len(), 5);
        assert_eq!(*pv.get(0), 7);
        assert_eq!(*pv.get(4), 9);

        pv.resize(2, &0); // shrink; fill value is unused and not inserted
        assert_eq!(pv.len(), 2);
        assert_eq!(pv.palette_len(), 2); // {7, 9} — shrink reclaimed nothing
        assert!((0..2).all(|i| *pv.get(i) == 7));
    }

    #[test]
    fn packed_iter_matches_get() {
        // 50 values at 3 bits spans multiple words (21 per word).
        let values: Vec<u64> = (0..50).map(|i| i % 8).collect();
        let pv = PackedVec::new(&values);
        assert_eq!(pv.iter().len(), 50); // ExactSizeIterator
        assert!(pv.iter().eq(values.iter().copied()));
        // matches get() element-wise
        assert!(pv.iter().enumerate().all(|(i, v)| v == pv.get(i)));
    }

    #[test]
    fn palette_iter_yields_values() {
        let values = vec![10u32, 20, 10, 30];
        let pv = PaletteVec::new(values.clone());
        let collected: Vec<u32> = pv.iter().copied().collect();
        assert_eq!(collected, values);
        assert_eq!(pv.iter().len(), 4);
    }

    #[test]
    fn into_iter_for_references() {
        let packed = PackedVec::new(&[3, 1, 2]);
        let mut sum = 0;
        for v in &packed {
            sum += v; // `for v in &packed` -> IntoIterator for &PackedVec
        }
        assert_eq!(sum, 6);

        let pal = PaletteVec::new(vec![5u32, 9, 5]);
        let collected: Vec<u32> = (&pal).into_iter().copied().collect();
        assert_eq!(collected, vec![5, 9, 5]);
        let mut count = 0;
        for _ in &pal {
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn set_bits_per_value_round_trips_grow_and_shrink() {
        let values: Vec<u64> = (0..100).map(|i| i % 8).collect(); // max 7 -> 3 bits
        let mut pv = PackedVec::new(&values);
        assert_eq!(pv.bits_per_value(), 3);
        // mix of grows and shrinks; 100 values leaves a partial final word at each width
        for &b in &[4usize, 8, 5, 3, 6, 16, 4, 3] {
            pv.set_bits_per_value(b);
            assert_eq!(pv.bits_per_value(), b);
            assert_eq!(pv.len(), values.len());
            for (i, &v) in values.iter().enumerate() {
                assert_eq!(pv.get(i), v, "value {i} after repack to {b} bits");
            }
        }
    }

    #[test]
    fn palette_single_value() {
        // uniform via construction -> palette len 1 -> O(1) path
        let uni = PaletteVec::new(vec![42u32; 10]);
        assert_eq!(uni.single_value(), Some(&42));

        let empty: PaletteVec<u32> = PaletteVec::empty();
        assert_eq!(empty.single_value(), None);

        let mixed = PaletteVec::new(vec![1u32, 1, 2, 1]);
        assert_eq!(mixed.single_value(), None);

        // uniform values but dead palette entries (len > 1) -> still detected via scan
        let mut dead = PaletteVec::new(vec![1u32, 2, 3]);
        for i in 0..3 {
            dead.set(i, &9);
        }
        assert!(dead.palette_len() > 1);
        assert_eq!(dead.single_value(), Some(&9));
    }

    #[test]
    fn packed_filled() {
        let pv = PackedVec::filled(50, 7); // 3 bits, partial final word
        assert_eq!(pv.len(), 50);
        assert_eq!(pv.bits_per_value(), 3);
        assert!((0..50).all(|i| pv.get(i) == 7));
        // canonical tail
        let vpw = 64 / pv.bits_per_value();
        let used_bits = (pv.len() - (pv.data.len() - 1) * vpw) * pv.bits_per_value();
        if used_bits < 64 {
            assert_eq!(*pv.data.last().unwrap() >> used_bits, 0);
        }

        let zeros = PackedVec::filled(10, 0);
        assert_eq!(zeros.bits_per_value(), 1);
        assert!((0..10).all(|i| zeros.get(i) == 0));

        assert_eq!(PackedVec::filled(0, 5).len(), 0);
    }

    #[test]
    fn palette_filled_is_uniform() {
        let pv = PaletteVec::filled(64, 99u32);
        assert_eq!(pv.len(), 64);
        assert_eq!(pv.palette_len(), 1);
        assert_eq!(pv.bits_per_value(), 1);
        assert_eq!(pv.single_value(), Some(&99)); // O(1) path
        assert!((0..64).all(|i| *pv.get(i) == 99));
    }

    #[test]
    fn bool_vec_basic() {
        let mut bv = BoolVec::new(&[true, false, true, true, false]);
        assert_eq!(bv.len(), 5);
        assert!(bv.get(0));
        assert!(!bv.get(1));
        assert_eq!(bv.iter().collect::<Vec<_>>(), vec![true, false, true, true, false]);
        assert_eq!(bv.count_ones(), 3);

        bv.set(1, true);
        assert!(bv.get(1));
        assert_eq!(bv.count_ones(), 4);

        bv.push(false);
        assert_eq!(bv.len(), 6);
        assert_eq!(bv.pop(), Some(false));
        assert_eq!(bv.len(), 5);
        assert_eq!(BoolVec::empty().pop(), None);
    }

    #[test]
    fn bool_vec_filled_resize_and_count() {
        let mut bv = BoolVec::filled(100, true); // spans two words, partial tail
        assert_eq!(bv.len(), 100);
        assert_eq!(bv.count_ones(), 100); // phantom tail bits must not be counted
        assert!((0..100).all(|i| bv.get(i)));

        bv.resize(150, false); // grow with false
        assert_eq!(bv.len(), 150);
        assert_eq!(bv.count_ones(), 100);
        assert!(!bv.get(149));

        bv.resize(50, false); // shrink
        assert_eq!(bv.len(), 50);
        assert_eq!(bv.count_ones(), 50);
    }

    #[test]
    fn bool_vec_into_iter() {
        let bv = BoolVec::new(&[true, true, false]);
        let trues = (&bv).into_iter().filter(|&b| b).count();
        assert_eq!(trues, 2);
        assert_eq!(bv.iter().len(), 3); // ExactSizeIterator
    }
}