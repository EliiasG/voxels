use std::array;

pub type BitMask4096 = NestedFixedBitMask<u64, u64, 64>;
pub type BitMask262144 = NestedFixedBitMask<u64, BitMask4096, 64>;

pub trait FixedBitMask {
    const BITS: u32;

    fn ones(&self) -> u32;

    fn zeroes(&self) -> u32;

    fn leading_zeros(&self) -> u32;
    fn trailing_zeros(&self) -> u32;
    fn leading_ones(&self) -> u32;
    fn trailing_ones(&self) -> u32;

    fn any(&self) -> bool;
    fn all(&self) -> bool;
    fn bit(&self, index: u32) -> bool;

    fn set_bit(&mut self, index: u32, value: bool);
}

pub struct NestedFixedBitMask<Outer: FixedBitMask, Inner: FixedBitMask, const N: usize> {
    outer_any: Outer,
    outer_all: Outer,
    inner: [Inner; N],
    ones: u32,
}

impl<Outer: FixedBitMask, Inner: FixedBitMask, const N: usize> NestedFixedBitMask<Outer, Inner, N> {
    /// Compile-time check that the inner array length matches the outer mask's
    /// bit count. Stable Rust can't use `Outer::BITS` directly as the array
    /// length, so `N` is supplied separately and validated here instead.
    const VALID_N: () = assert!(N == Outer::BITS as usize, "N must equal Outer::BITS");

    /// recalculates outer_any and outer_all
    pub fn from_values(outer_any: Outer, outer_all: Outer, inner: [Inner; N]) -> Self {
        // Force `VALID_N` to be evaluated for this monomorphization.
        let () = Self::VALID_N;
        let mut res = Self {
            outer_any,
            outer_all,
            inner,
            ones: 0,
        };
        // Ensure res is valid
        res.recalculate();
        res
    }

    #[inline(always)]
    fn recalculate_outer_indexed(&mut self, index: u32) {
        self.outer_all
            .set_bit(index, self.inner[index as usize].all());
        self.outer_any
            .set_bit(index, self.inner[index as usize].any());
    }

    fn recalculate(&mut self) {
        self.ones = 0;
        for i in 0..Outer::BITS {
            self.recalculate_outer_indexed(i);
            self.ones += self.inner[i as usize].ones();
        }
    }
}

impl<Outer: FixedBitMask + Default, Inner: FixedBitMask + Default, const N: usize>
    NestedFixedBitMask<Outer, Inner, N>
{
    pub fn new() -> Self {
        Default::default()
    }
}

impl<Outer: FixedBitMask + Default, Inner: FixedBitMask + Default, const N: usize> Default
    for NestedFixedBitMask<Outer, Inner, N>
{
    fn default() -> Self {
        Self::from_values(
            Default::default(),
            Default::default(),
            array::from_fn(|_| Default::default()),
        )
    }
}

impl<Outer: FixedBitMask, Inner: FixedBitMask, const N: usize> FixedBitMask
    for NestedFixedBitMask<Outer, Inner, N>
{
    const BITS: u32 = Outer::BITS * Inner::BITS;

    #[inline(always)]
    fn ones(&self) -> u32 {
        // Maintained incrementally by `set_bit` and `recalculate`.
        self.ones
    }

    #[inline(always)]
    fn zeroes(&self) -> u32 {
        Self::BITS - self.ones
    }

    #[inline]
    fn leading_zeros(&self) -> u32 {
        // `outer_any` bit `o` is set iff chunk `o` is non-empty, so its leading
        // zeros count the empty chunks at the most-significant end.
        let empty_chunks = self.outer_any.leading_zeros();
        if empty_chunks == Outer::BITS {
            // Every chunk is empty: the whole mask is zero.
            return Self::BITS;
        }
        // The most-significant non-empty chunk; scan within it for the rest.
        let chunk = Outer::BITS - 1 - empty_chunks;
        empty_chunks * Inner::BITS + self.inner[chunk as usize].leading_zeros()
    }

    #[inline]
    fn trailing_zeros(&self) -> u32 {
        // Trailing zeros of `outer_any` count the empty chunks at the
        // least-significant end.
        let empty_chunks = self.outer_any.trailing_zeros();
        if empty_chunks == Outer::BITS {
            // Every chunk is empty: the whole mask is zero.
            return Self::BITS;
        }
        // Chunk `empty_chunks` is the least-significant non-empty one.
        empty_chunks * Inner::BITS + self.inner[empty_chunks as usize].trailing_zeros()
    }

    #[inline]
    fn leading_ones(&self) -> u32 {
        // `outer_all` bit `o` is set iff chunk `o` is completely full, so its
        // leading ones count the full chunks at the most-significant end.
        let full_chunks = self.outer_all.leading_ones();
        if full_chunks == Outer::BITS {
            // Every chunk is full: the whole mask is all ones.
            return Self::BITS;
        }
        // The most-significant non-full chunk; scan within it for the rest.
        let chunk = Outer::BITS - 1 - full_chunks;
        full_chunks * Inner::BITS + self.inner[chunk as usize].leading_ones()
    }

    #[inline]
    fn trailing_ones(&self) -> u32 {
        // Trailing ones of `outer_all` count the full chunks at the
        // least-significant end.
        let full_chunks = self.outer_all.trailing_ones();
        if full_chunks == Outer::BITS {
            // Every chunk is full: the whole mask is all ones.
            return Self::BITS;
        }
        // Chunk `full_chunks` is the least-significant non-full one.
        full_chunks * Inner::BITS + self.inner[full_chunks as usize].trailing_ones()
    }

    #[inline(always)]
    fn any(&self) -> bool {
        // Any bit is set iff some chunk is non-empty.
        self.outer_any.any()
    }

    #[inline(always)]
    fn all(&self) -> bool {
        // All bits are set iff every chunk is full.
        self.outer_all.all()
    }

    #[inline(always)]
    fn bit(&self, index: u32) -> bool {
        debug_assert!(index < Self::BITS, "bit index {index} out of range");
        let outer_index = index / Inner::BITS;
        let inner_index = index % Inner::BITS;
        self.inner[outer_index as usize].bit(inner_index)
    }

    #[inline]
    fn set_bit(&mut self, index: u32, value: bool) {
        debug_assert!(index < Self::BITS, "bit index {index} out of range");
        let outer_index = index / Inner::BITS;
        let inner_index = index % Inner::BITS;
        // Branchless popcount delta: +1 when setting a clear bit, -1 when
        // clearing a set bit, 0 otherwise. Evaluated left-to-right so the
        // subtraction can't underflow (`self.ones` already counts the old bit).
        self.ones += value as u32 - self.inner[outer_index as usize].bit(inner_index) as u32;
        self.inner[outer_index as usize].set_bit(inner_index, value);
        // Keep the outer summaries in sync with the chunk we just touched.
        self.recalculate_outer_indexed(outer_index);
    }
}

impl FixedBitMask for u64 {
    const BITS: u32 = u64::BITS;

    #[inline(always)]
    fn ones(&self) -> u32 {
        u64::count_ones(*self)
    }

    #[inline(always)]
    fn zeroes(&self) -> u32 {
        u64::count_zeros(*self)
    }

    #[inline(always)]
    fn leading_zeros(&self) -> u32 {
        u64::leading_zeros(*self)
    }
    #[inline(always)]
    fn trailing_zeros(&self) -> u32 {
        u64::trailing_zeros(*self)
    }
    #[inline(always)]
    fn leading_ones(&self) -> u32 {
        u64::leading_ones(*self)
    }
    #[inline(always)]
    fn trailing_ones(&self) -> u32 {
        u64::trailing_ones(*self)
    }

    #[inline(always)]
    fn any(&self) -> bool {
        *self != 0
    }
    #[inline(always)]
    fn all(&self) -> bool {
        *self == u64::MAX
    }

    #[inline(always)]
    fn bit(&self, index: u32) -> bool {
        debug_assert!(index < Self::BITS, "bit index {index} out of range");
        (*self >> index) & 1 != 0
    }

    #[inline(always)]
    fn set_bit(&mut self, index: u32, value: bool) {
        debug_assert!(
            index < <Self as FixedBitMask>::BITS,
            "bit index {index} out of range for u64"
        );
        let bit = 1u64 << index;
        if value {
            *self |= bit;
        } else {
            *self &= !bit;
        }
    }
}
