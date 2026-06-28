//! [`SphereChunkSubscriber`]: a spherical chunk loader.
//!
//! Keeps every LOD in `0..lods` resident as a full sphere of radius `radius_end`
//! (in that LOD's own chunk units — clipmap style, so coarser LODs reach
//! exponentially farther in world space). The set is streamed as subscribe/
//! unsubscribe deltas rather than rescanned (CHUNK_LOADING_NOTES §3): the held set
//! is kept equal to `inside(center, radius_end)` per LOD as an invariant, so the
//! unsubscribe delta is provably a subset of what was subscribed — no per-chunk
//! membership check is needed.
//!
//! `radius_begin`/`radius_step`/`lod_order` do **not** change *what* is resident,
//! only the *order* chunks are offered to the generator (§4 priority frontier):
//! shell-by-shell outward (`begin`, `+step`, … clamped to `end`), and within each
//! shell across LODs in `lod_order`.
//!
//! Current params live on the public [`SphereChunkSubscriber`] + [`ChunkPosition`];
//! the last *applied* residency params are snapshotted in the private [`AppliedState`].
//! Diffing new-vs-applied makes any param change (move, grow, more LODs) correct.

use crate::chunk::manager::{
    ChunkList, ChunkSubscribeMessage, ChunkSubscriber, ChunkUnsubscribeMessage, SubscriberEntity,
};
use crate::chunk::{ChunkPosition, MAX_LOD_COUNT};
use bevy::prelude::*;

#[derive(Copy, Clone)]
pub enum LodOrder {
    LowToHigh,
    HighToLow,
}

#[derive(Component, Copy, Clone)]
#[require(ChunkSubscriber, ChunkPosition)]
pub struct SphereChunkSubscriber {
    /// Radius of the innermost load-order shell (chunks within it load first).
    pub radius_begin: usize,
    /// Residency radius: every active LOD is held resident out to here.
    pub radius_end: usize,
    /// Width of each successive load-order shell.
    pub radius_step: usize,
    /// Number of active LODs (buckets emitted for LODs `0..lods`).
    pub lods: usize,
    /// LOD order within a shell.
    pub lod_order: LodOrder,
}

/// Snapshot of the residency params the held set currently reflects. Private: it is
/// the subscriber's only state — the `last_chunk_pos` cache of §3 generalised to all
/// residency-affecting params. Load-order params are absent on purpose: changing them
/// alone is not a residency change.
#[derive(Component)]
struct AppliedState {
    center: IVec3,
    radius: i32,
    lods: usize,
}

pub struct SphereSubscriberPlugin;

impl Plugin for SphereSubscriberPlugin {
    fn build(&self, app: &mut App) {
        // The loading systems read these too; `add_message` is idempotent, so this is
        // safe regardless of plugin order. Order before `schedule_generation` for
        // same-tick delivery (messages persist a tick anyway).
        app.add_systems(FixedUpdate, update_sphere_subscribers);
    }
}

fn update_sphere_subscribers(
    mut commands: Commands,
    mut subscribe: MessageWriter<ChunkSubscribeMessage>,
    mut unsubscribe: MessageWriter<ChunkUnsubscribeMessage>,
    mut subscribers: Query<(
        Entity,
        &SphereChunkSubscriber,
        &ChunkPosition,
        Option<&mut AppliedState>,
    )>,
) {
    for (entity, sphere, pos, mut applied) in &mut subscribers {
        let new_center = pos.0;
        let new_radius = sphere.radius_end as i32;
        // clamp so a stray `lods` can't index past the chunk index's maps
        let new_lods = sphere.lods.min(MAX_LOD_COUNT);

        // EMPTY until bootstrapped: radius -1 is the empty sphere, so the diff yields
        // full spheres to subscribe and nothing to unsubscribe.
        let (old_center, old_radius, old_lods) = match applied.as_ref() {
            Some(a) => (a.center, a.radius, a.lods),
            None => (IVec3::ZERO, -1, 0),
        };

        // Residency depends only on (center, radius_end, lods). A change to load-order
        // params alone leaves the resident set untouched → nothing to stream.
        if new_center == old_center && new_radius == old_radius && new_lods == old_lods {
            continue;
        }

        let lod_span = old_lods.max(new_lods);

        // --- subscribe: per-LOD crescent, grouped into load-order buckets ---
        let sub_buckets = build_subscribe_buckets(
            sphere, lod_span, new_center, new_radius, new_lods, old_center, old_radius, old_lods,
        );
        if !sub_buckets.is_empty() {
            subscribe.write(ChunkSubscribeMessage {
                subscriber: SubscriberEntity(entity),
                buckets: sub_buckets,
            });
        }

        // --- unsubscribe: per-LOD crescent; load order is irrelevant here ---
        let mut unsub_buckets = Vec::new();
        for lod in 0..lod_span {
            let shift = lod as u32;
            let (old_c, old_r) = lod_sphere(old_center, old_radius, old_lods, lod, shift);
            let (new_c, new_r) = lod_sphere(new_center, new_radius, new_lods, lod, shift);
            let mut chunks = Vec::new();
            // inside(old) \ inside(new): provably a subset of what we held, so no check
            sphere_difference(old_c, old_r, new_c, new_r, &mut chunks);
            if !chunks.is_empty() {
                unsub_buckets.push(ChunkList { lod, chunks });
            }
        }
        if !unsub_buckets.is_empty() {
            unsubscribe.write(ChunkUnsubscribeMessage {
                buckets: unsub_buckets,
            });
        }

        // commit: the held set now reflects the new residency params
        match applied.as_mut() {
            Some(a) => {
                a.center = new_center;
                a.radius = new_radius;
                a.lods = new_lods;
            }
            None => {
                commands.entity(entity).insert(AppliedState {
                    center: new_center,
                    radius: new_radius,
                    lods: new_lods,
                });
            }
        }
    }
}

/// The sphere a given LOD occupies: center floor-shifted into LOD space, radius in that
/// LOD's own units. LODs outside the active range get the empty sphere (radius -1).
#[inline]
fn lod_sphere(center: IVec3, radius: i32, lods: usize, lod: usize, shift: u32) -> (IVec3, i32) {
    if lod < lods {
        (shr(center, shift), radius)
    } else {
        (IVec3::ZERO, -1)
    }
}

/// Build the subscribe message's buckets in load order: shell-major (inner shells
/// first), within each shell across LODs per `lod_order`, nearest-first inside a bucket.
/// Each bucket is one (shell, LOD) pair — so the same LOD appears once per shell it spans.
fn build_subscribe_buckets(
    sphere: &SphereChunkSubscriber,
    lod_span: usize,
    new_center: IVec3,
    new_radius: i32,
    new_lods: usize,
    old_center: IVec3,
    old_radius: i32,
    old_lods: usize,
) -> Vec<ChunkList> {
    let begin = sphere.radius_begin as i32;
    let step = (sphere.radius_step.max(1)) as i32; // guard against a 0 step

    // (shell, lod_rank, dist², lod, pos); sorting by the first three gives load order.
    let mut entries: Vec<(i32, usize, i32, usize, IVec3)> = Vec::new();
    for lod in 0..lod_span {
        let shift = lod as u32;
        let (new_c, new_r) = lod_sphere(new_center, new_radius, new_lods, lod, shift);
        if new_r < 0 {
            continue; // LOD not active in the new params → nothing to subscribe
        }
        let (old_c, old_r) = lod_sphere(old_center, old_radius, old_lods, lod, shift);

        let mut chunks = Vec::new();
        sphere_difference(new_c, new_r, old_c, old_r, &mut chunks);

        let rank = match sphere.lod_order {
            LodOrder::LowToHigh => lod,
            LodOrder::HighToLow => lod_span - 1 - lod,
        };
        for p in chunks {
            let d2 = (p - new_c).length_squared();
            let shell = shell_index(d2.isqrt(), begin, step);
            entries.push((shell, rank, d2, lod, p));
        }
    }

    entries.sort_unstable_by_key(|&(shell, rank, d2, _, _)| (shell, rank, d2));

    // group runs of equal (shell, rank) — i.e. equal (shell, lod) — into one bucket
    let mut buckets = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let (shell, rank, _, lod, _) = entries[i];
        let mut chunks = Vec::new();
        while i < entries.len() && entries[i].0 == shell && entries[i].1 == rank {
            chunks.push(entries[i].4);
            i += 1;
        }
        buckets.push(ChunkList { lod, chunks });
    }
    buckets
}

/// Which load-order shell a chunk at integer distance `dist` falls in. Shell 0 is the
/// ball out to `begin`; each later shell is `step` wide.
#[inline]
fn shell_index(dist: i32, begin: i32, step: i32) -> i32 {
    if dist <= begin {
        0
    } else {
        1 + (dist - begin - 1) / step
    }
}

/// Floor-divide each component by `2^shift` — maps a LOD0 chunk coord into LOD-`shift`
/// space. Arithmetic right shift is floor division, which is correct for negatives.
#[inline]
fn shr(c: IVec3, shift: u32) -> IVec3 {
    IVec3::new(c.x >> shift, c.y >> shift, c.z >> shift)
}

/// Append every chunk inside `sphere(c_in, r_in)` but **not** inside `sphere(c_ex, r_ex)`.
///
/// A sphere is `{ p : |p - c|² ≤ r² }` in chunk coords; a negative radius is the empty
/// set. Iterates the include disk column-by-column (≈ π·r_in² columns) and emits each
/// column's z-interval minus the exclude sphere's z-interval there — O(r²), never O(r³),
/// and correct for any center/radius pair (move, grow, shrink, or all at once).
fn sphere_difference(c_in: IVec3, r_in: i32, c_ex: IVec3, r_ex: i32, out: &mut Vec<IVec3>) {
    if r_in < 0 {
        return;
    }
    let r_in2 = r_in * r_in;
    let r_ex2 = r_ex * r_ex;

    for dx in -r_in..=r_in {
        let x = c_in.x + dx;
        let rem_x = r_in2 - dx * dx; // ≥ 0 since dx² ≤ r_in²
        let dy_max = rem_x.isqrt();
        for dy in -dy_max..=dy_max {
            let y = c_in.y + dy;
            let z_half = (rem_x - dy * dy).isqrt(); // ≥ 0
            let in_lo = c_in.z - z_half;
            let in_hi = c_in.z + z_half;

            // exclude sphere's z-interval at this (x, y); (1, 0) encodes "empty"
            let (ex_lo, ex_hi) = if r_ex >= 0 {
                let edx = x - c_ex.x;
                let edy = y - c_ex.y;
                let rem = r_ex2 - edx * edx - edy * edy;
                if rem >= 0 {
                    let h = rem.isqrt();
                    (c_ex.z - h, c_ex.z + h)
                } else {
                    (1, 0)
                }
            } else {
                (1, 0)
            };

            // emit [in_lo, in_hi] \ [ex_lo, ex_hi]
            if ex_lo > ex_hi || ex_hi < in_lo || ex_lo > in_hi {
                for z in in_lo..=in_hi {
                    out.push(IVec3::new(x, y, z));
                }
            } else {
                for z in in_lo..=(ex_lo - 1).min(in_hi) {
                    out.push(IVec3::new(x, y, z));
                }
                for z in (ex_hi + 1).max(in_lo)..=in_hi {
                    out.push(IVec3::new(x, y, z));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn inside(c: IVec3, r: i32, p: IVec3) -> bool {
        r >= 0 && (p - c).length_squared() <= r * r
    }

    fn brute_diff(c_in: IVec3, r_in: i32, c_ex: IVec3, r_ex: i32) -> HashSet<IVec3> {
        let mut set = HashSet::new();
        let r = r_in.max(0);
        for x in (c_in.x - r)..=(c_in.x + r) {
            for y in (c_in.y - r)..=(c_in.y + r) {
                for z in (c_in.z - r)..=(c_in.z + r) {
                    let p = IVec3::new(x, y, z);
                    if inside(c_in, r_in, p) && !inside(c_ex, r_ex, p) {
                        set.insert(p);
                    }
                }
            }
        }
        set
    }

    fn diff_set(c_in: IVec3, r_in: i32, c_ex: IVec3, r_ex: i32) -> HashSet<IVec3> {
        let mut v = Vec::new();
        sphere_difference(c_in, r_in, c_ex, r_ex, &mut v);
        let set: HashSet<_> = v.iter().copied().collect();
        assert_eq!(set.len(), v.len(), "sphere_difference emitted duplicates");
        set
    }

    fn full_sphere(c: IVec3, r: i32) -> HashSet<IVec3> {
        brute_diff(c, r, IVec3::ZERO, -1)
    }

    #[test]
    fn difference_matches_brute_force() {
        let cases = [
            (IVec3::ZERO, 5, IVec3::ZERO, -1),                   // bootstrap: full sphere
            (IVec3::ZERO, 5, IVec3::ZERO, 5),                    // identical → empty
            (IVec3::new(1, 0, 0), 6, IVec3::ZERO, 6),            // unit move crescent
            (IVec3::new(2, -3, 1), 7, IVec3::ZERO, 6),           // move + grow
            (IVec3::ZERO, 4, IVec3::new(1, 1, 0), 7),            // shrink relative
            (IVec3::new(-4, 2, -1), 8, IVec3::new(3, -2, 5), 3), // mostly disjoint
        ];
        for (ci, ri, ce, re) in cases {
            assert_eq!(
                diff_set(ci, ri, ce, re),
                brute_diff(ci, ri, ce, re),
                "case in={ci:?} r={ri} ex={ce:?} r={re}"
            );
        }
    }

    #[test]
    fn move_keeps_the_residency_invariant() {
        // held(new) == held(old) − unsubscribe + subscribe == inside(new)
        let (old_c, new_c, r) = (IVec3::ZERO, IVec3::new(1, 1, 0), 6);
        let subscribe = diff_set(new_c, r, old_c, r);
        let unsubscribe = diff_set(old_c, r, new_c, r);

        // every unsubscribed chunk was previously held (subset of the old sphere)
        for p in &unsubscribe {
            assert!(inside(old_c, r, *p), "unsubscribed a never-held chunk: {p:?}");
        }

        let mut held = full_sphere(old_c, r);
        for p in &unsubscribe {
            held.remove(p);
        }
        held.extend(subscribe);
        assert_eq!(held, full_sphere(new_c, r));
    }

    #[test]
    fn shells_bin_by_radius() {
        // begin=3, step=2: shell 0 = [0,3], shell 1 = (3,5], shell 2 = (5,7], …
        assert_eq!(shell_index(0, 3, 2), 0);
        assert_eq!(shell_index(3, 3, 2), 0);
        assert_eq!(shell_index(4, 3, 2), 1);
        assert_eq!(shell_index(5, 3, 2), 1);
        assert_eq!(shell_index(6, 3, 2), 2);
        assert_eq!(shell_index(7, 3, 2), 2);
    }

    #[test]
    fn bootstrap_buckets_are_ordered_and_complete() {
        let sphere = SphereChunkSubscriber {
            radius_begin: 2,
            radius_end: 5,
            radius_step: 2,
            lods: 3,
            lod_order: LodOrder::LowToHigh,
        };
        // bootstrap: old is empty (radius -1, 0 lods)
        let buckets = build_subscribe_buckets(&sphere, 3, IVec3::ZERO, 5, 3, IVec3::ZERO, -1, 0);

        // buckets strictly increase in (shell, lod) — shell-major, LOD-ascending
        let mut prev: Option<(i32, usize)> = None;
        for b in &buckets {
            let center_l = shr(IVec3::ZERO, b.lod as u32);
            let shells: HashSet<i32> = b
                .chunks
                .iter()
                .map(|p| shell_index((*p - center_l).length_squared().isqrt(), 2, 2))
                .collect();
            assert_eq!(shells.len(), 1, "a bucket must sit in exactly one shell");
            let shell = *shells.iter().next().unwrap();
            if let Some(prev) = prev {
                assert!((shell, b.lod) > prev, "buckets out of order: {:?} after {prev:?}", (shell, b.lod));
            }
            prev = Some((shell, b.lod));
        }

        // every LOD's chunks across all buckets reconstruct its full sphere
        for lod in 0..3usize {
            let got: HashSet<IVec3> = buckets
                .iter()
                .filter(|b| b.lod == lod)
                .flat_map(|b| b.chunks.iter().copied())
                .collect();
            assert_eq!(got, full_sphere(shr(IVec3::ZERO, lod as u32), 5), "LOD {lod} coverage");
        }
    }
}
