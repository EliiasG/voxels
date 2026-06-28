# Chunk Loading & Streaming Design Notes

> Design discussion captured 2026-06-19. Target: Rust + Bevy-ECS (no `bevy_render`;
> own winit+wgpu) voxel engine. 3D chunk map, `CHUNK_SIZE = 32`, up to `MAX_LOD_COUNT`
> LODs, large render distance, must stay smooth on low-end iGPUs while a 20 Hz
> simulation streams/generates/meshes thousands of chunks. MP-capable (bevy_replicon),
> but designed single-player-shaped. Scope here excludes the netcode internals and
> persistence of edits — only the streaming/loading architecture.

## Guiding thesis

**The framerate loop must never wait on bulk chunk work.** Every decision below exists
to keep heavy, latency-tolerant work (gen, meshing, bookkeeping over tens of thousands
of chunks) off the path that draws the next frame and responds to input — without
forking single-player and multiplayer code paths.

Two recurring tools do most of the work:
1. **The two-path split** — *bulk* work is async, budgeted, latency-tolerant, high
   throughput; *interactive* work is synchronous, immediate, single-item, low
   throughput. They never share a code path.
2. **Synchronize at chunk granularity, not world granularity** — `Arc<ChunkStorage>`
   copy-on-write gives lock-free snapshot reads; locks are held only for a pointer
   swap, never across heavy compute.

---

## 0. The two worlds (the spine)

The engine runs **two ECS worlds on two threads**:

- **Sim world (20 Hz)** — *produces the world*. Owns terrain gen, the edit overlay,
  entity simulation, the chunk index/refcounts, and the chunk **production pipeline**
  (gen → mesh). Wears a **client or server hat** depending on connection.
- **Gameplay world (framerate)** — *presents & interacts with the world*. Owns the
  camera, input, prediction, GPU upload, drawing, and the instant-edit fast-path.

**Why two worlds (and why this isn't quite "internal server"):** the split puts the
sim world's heavy 20 Hz work on its own thread, so it runs *concurrently* with the
framerate loop — the gameplay world reads its own state and never stalls on a long
tick or a lock. It also matches the Replicon MP commitment so SP and MP don't fork.
But it is **not** a classic integrated server: **connected clients generate their own
terrain locally** (see §1), so the 20 Hz world is *the client* when connected to a
remote server — not a server the client talks to.

**This is the fix to the original "FixedUpdate stall" problem.** Bevy's schedules run
sequentially inside one `app.update()`; a >10 ms `FixedUpdate` would delay `Update`
(the frame) by that much, and FixedUpdate's catch-up multiplies it. Putting the sim on
its own thread + keeping heavy work on pools (§6, §7) removes the sim from the frame's
critical path entirely.

---

## 1. Terrain is local, the overlay is shared

**Base terrain is never replicated — it is regenerated locally and deterministically
on every node.** Only the **edit overlay** (block changes) and **entity state**
(inventories, machines, mobs, players, damage) sync over the network.

> sim world state  =  local-generated base terrain  +  authoritative edit overlay  +  entity sim

- The **server is authoritative over the overlay and entities, not the base.**
- Huge bandwidth win: no chunk-transfer protocol; terrain costs a seed.
- Makes gen a **first-class local job of every sim world**, identical everywhere.

**The load-bearing dependency: cross-node gen determinism.** If gen touches floats,
platform/compiler/SIMD divergence makes client A see stone where B sees air → edits and
collisions desync. This must be handled (fixed-point gen, a determinism-controlled
float path, or server-reconciles-divergence). Open: which (§11).

---

## 2. Chunk loaders, the desired set, and refcounting

A **loader** declares "keep chunks resident around me." It is a subscription the
**gameplay world sends to the sim world** (camera position + radius + LOD field) — the
same message in SP and MP. The sim world owns the residency bookkeeping.

```rust
ChunkLoader { detail_radius, last_chunk_pos }   // world pos via Transform; LOD count global
```

- The loader's `Entity` is its identity — **no explicit ID field** (that was for a
  subscriber list we don't keep).
- `last_chunk_pos` is the boundary-crossing cache (§3), not gameplay state.
- LOD count stays **global** (the index already holds one) until a second camera needs
  genuinely different LODs.

**Refcount, not derive — because a client can have multiple cameras.** With several
loaders sharing chunks, a stored refcount avoids recomputing "who wants this" every
tick and handles set-union ownership cleanly (unload at 0). A **count, never a list** —
we never need to enumerate *who* keeps a chunk alive, only *whether* anyone does.

**Fold refcount + load-state + residency into one slot**, so "is it loaded already?"
*is* the same probe that owns refcounting:

```rust
struct ChunkSlot {
    refs: u16,                  // unload when this hits 0
    state: ChunkState,          // Loading (gen in flight) | Loaded
    data: Option<Arc<ChunkStorage>>,   // None until gen completes
    // entity handle(s) for render-side components live alongside or on the entity
}
// ChunkIndex: [HashMap<IVec3, ChunkSlot>; MAX_LOD_COUNT]   (keys are per-LOD coords)
```

A chunk's slot exists from first-desired (`refs:1, Loading, None`) through gen to
`Loaded`. One `entry(coord)` does everything: absent → insert + push to the gen
frontier; present → `refs += 1`. **Race guard:** when gen completes, check `refs > 0`
before installing — a camera may have left mid-gen → drop the result.

---

## 3. Streaming by delta, never by full scan

A per-tick scan of the whole desired set is the wrong model regardless of how fast each
lookup is. **Touch only the chunks that changed.**

- A loader that didn't cross a chunk boundary this tick costs **zero** (compare current
  chunk-coord to `last_chunk_pos`).
- On crossing, a one-chunk move exposes/hides a thin **cap** ≈ `π·r²` chunks (≈700 at
  r=15), not the whole ball — inc/dec those slots.
- **Higher LODs cross 2^k× less often** (a LOD*k* chunk is 2^k× wider), so amortized
  cost is dominated by LOD0 and **scales with camera speed, not render distance³.**

Order-of-magnitude (r=15, ~50 ns/probe): naive full scan ≈ 30 000 × 20 Hz ≈ **~30 ms/s
of pure overhead**; delta at ~5 chunks/s ≈ **sub-millisecond**, shared across cameras.

**Edge cases:**
- **Δ > 1 per tick** (fast camera at 20 Hz) — the cap enumeration must handle multi-step
  jumps, not assume single-step.
- **Teleport / spawn** — rare full path: dec-all-old, inc-all-new.
- **New camera ≠ spike.** Computing its desired set is a one-time ~1–1.5 ms refcount
  enumeration (spread inner-shells-first if it ever shows in a profile); *materializing*
  is async + budgeted, so the camera streams in like game startup. A camera spawning
  *near* an existing one is nearly free — overlapping chunks already resident, just bump
  refs; only the non-overlapping crescent streams.

**Number correction:** a true sphere at r=15 is `4/3·π·15³ ≈ 14 000` chunks per level,
not 3 000 (3 000 ≈ r≈9, or a vertically-flattened volume). Budget memory accordingly.

---

## 4. Load order: the priority frontier

**Separate membership from order.** Membership (is this chunk desired, at what LOD) =
concentric shells / clipmap. Order (which desired-but-missing chunk to materialize next)
= a **priority key** on a frontier the generator surveys. Changing loading *feel* must
not touch *correctness*.

- The diff emits a **set with priorities**, not a one-at-a-time stream — so the
  generator can batch by locality (shared large-scale noise, caves/veins/structures
  across borders, column-wise fields) and so meshing has neighbor data.
- **Priority = world-space distance** gives the staged expansion we want *for free*:
  the resident region is always a complete bubble out to its current radius and grows
  outward as the budget drains. **No explicit multi-phase "all-LODs-then-grow" state
  machine** — that behavior *is* nearest-first over concentric shells. Detail-on-approach
  is automatic too (shells re-bin to finer LODs as the loader moves).

**One real fork (feel, not architecture — a one-line key change):**
- **Near-first** (bubble grows outward): simplest/cheapest; hard edge expands; void
  beyond it until reached.
- **Coarse-skeleton-first** (whole vista appears blurry, sharpens inward): instant sense
  of place to max distance — matches the renderer's "sell the distance" thesis. Costs:
  far coarse LODs generate first and may never refine while moving.

Open (§11); leans coarse-first given the renderer is built to sell distance.

---

## 5. Residency: stacked clipmap vs concentric shells

The ~30 k resident chunks (10 LODs × ~3 k) are **intended, not a problem to shrink** —
the fix is to never *scan* them (§3), not to make them fewer.

- **Stacked clipmap** (coarse LODs resident *under* fine ones, ~10×): free instant LOD
  transitions (coarse data already there when fine unloads) + coarse data on hand for
  CSM carving / GI far-occluder queries (see RENDERING_NOTES). This is what yields ~30 k.
- **Concentric shells** (one LOD per region, ~one sphere total): ~halves memory, but LOD
  transitions must generate the coarser chunk just-in-time → pop risk, and no
  coarse-under-fine for shadows.

RENDERING_NOTES' CSM carving leans toward needing some coarse-under-fine → stacked.
Open (§11). If residency lookups ever genuinely dominate (they won't, with deltas), the
clipmap escape hatch is **toroidal dense arrays** instead of hashmaps (O(1), no hashing)
— a later lever, not now.

---

## 6. The production pipeline (sim world owns gen → mesh)

The sim world owns the chunk **production pipeline end-to-end**, both stages on worker
pools:

- **Gen (voxels)** — sim world's job. On a pool, so the 20 Hz tick stays short and
  streaming stays responsive (a long tick delays replication, not just gen).
- **Mesh (voxels → CPU vertex/index buffers)** — also the sim world's job. **This is a
  correction to an earlier guess that meshing belongs in the gameplay world.** Meshing
  belongs in the sim world because:
  - **Remesh is triggered by voxel changes** (gen completes, edit lands) — events that
    *originate* in the sim world. Gameplay would have to be told; the sim world knows.
  - **Meshing needs neighbor voxels** — the sim world owns all of them.
  - **The LOD to mesh at is the same loader/LOD field that already drives gen** — gen
    and mesh become one pipeline keyed on the camera subscription.

Result: the sim world produces (voxels + meshes); the **gameplay world shrinks to
upload + draw + input + predict** — a smaller smooth-world surface = fewer places for an
accidental O(n) scan to creep onto the frame path. (Caveat: the dominant smoothness
lever is still "heavy work on pools + bounded main-thread work"; moving meshing refines
that, it doesn't replace it.)

**Lock discipline (the rule that makes it safe):** never hold a chunk lock across heavy
compute. A mesher locks the slot only to `Arc::clone` the snapshot out, *releases*,
meshes for ~10 ms on the clone, then briefly re-locks to install the result (after
`refs > 0`). Lock hold time ≈ a pointer swap, independent of mesh cost.

---

## 7. Concurrency: why nothing blocks the frame

**Keep `ChunkData(Arc<ChunkStorage>)` as a component** — the ECS composition is wanted
(attach mesh handles, flags, etc.), and the `Arc` is exactly the lock-free escape hatch:

- Each world reads **its own** chunk entities on **its own** thread — no cross-world
  borrow to serialize.
- **Copy-on-write per chunk:** a writer (gen result, edit) builds a new `ChunkStorage`
  for the one affected chunk and **swaps the `Arc`**; readers (raycast, mesher) clone a
  stable snapshot and never block the writer. Worst case a reader sees the pre-edit
  version for one frame.
- The voxel grid is therefore synchronized at **chunk granularity** (a brief swap), not
  at **world granularity** (Bevy's coarse, serial, in-schedule access). Heavy work lives
  on pools, off both the frame and (via §6) the tick.

**FixedUpdate hygiene on the sim side:** keep the in-schedule body O(budget) — compute
deltas, enqueue pool work, collect ≤N results. The 10 ms work is on pools, never inline,
so the catch-up death-spiral can't start. Size **two budgets**: cold-start (a full
sphere — loading screen or temporarily large) vs steady-state (a thin ~700-chunk
frontier — small); tune the steady one.

---

## 8. The two-path principle & instant edit feedback

Routing *everything* through the 20 Hz sim world would make a block break round-trip
≥50 ms before it even schedules a remesh. So split by latency/throughput:

- **Streaming / bulk meshes → sim world** (high throughput; 50 ms granularity is
  invisible at distance — and the completed-mesh channel is drained by gameplay **every
  frame**, so streaming latency isn't tied to the tick clock).
- **The chunk the player just edited → immediate local remesh in the gameplay world**
  (one chunk, sub-ms, instant feel). The sim world's authoritative remesh replaces it a
  tick later — identical geometry in SP, reconciled in MP. Bounded redundancy (one
  chunk, on a rare action) bought for instant feedback.

---

## 9. Cross-world communication

- **gameplay → sim:** loader subscription (camera pos / radius / LOD), commands (edit,
  interact, take-item, deal-damage).
- **sim → gameplay:** ready CPU meshes (channel, drained per frame), replicated entity
  state, edit confirmations.
- **GPU upload stays on the render thread.** The sim pool produces *CPU* vertex/index
  buffers; the gameplay world allocates + uploads to wgpu (budgeted) and draws — because
  the wgpu queue lives render-side. (Sharing an `Arc<Queue>` to upload from the task is
  possible; allocation/ordering are simpler render-side — revisit only if profiling
  wants it.)
- **Client-gate the mesher.** A dedicated/headless server runs the same sim world *minus*
  the meshing systems (run-condition / feature gate). This is the only place "servers
  don't mesh" still bites — handled by gating, not by world placement.

---

## 10. Scope / build order

**Build the seam now, defer the hard netcode behind it.**

- Now (cheap, load-bearing): the two-world structure (sim on its own thread), the
  command/event channels, the loader subscription, the `ChunkSlot` index + refcounts,
  delta streaming, the gen→mesh pipeline on pools, the `Arc` CoW store, the per-frame
  mesh-upload drain, the instant-edit fast-path.
- Defer (behind the seam): real prediction/reconciliation, edit persistence, chunk/edit
  serialization for remote MP, and gen-determinism hardening. Don't build rollback
  netcode before terrain is on screen.

---

## 11. Open questions / pending decisions

- **Load order: near-first vs coarse-skeleton-first** (§4). One-line priority key; leans
  coarse-first.
- **Residency: stacked clipmap vs concentric shells** (§5) — the memory/feature driver;
  leans stacked for CSM carving.
- **SP edit path:** write the shared store directly (instant, gameplay applies) vs
  round-trip through the sim world (≤50 ms, strictly sim-authoritative). Sets how much
  prediction machinery is needed later.
- **Gen determinism strictness** (§1): fixed-point / controlled-float / reconcile.
- **Per-loader LOD count** — global for now; revisit if a second camera needs different
  LODs.
- **Render distance / LOD count tuning** — r=15 at LOD9 reaches absurd world distance;
  may want fewer LODs or smaller radius at coarse levels (clipmaps normally keep radius
  constant per level).
- **Lock primitive for the store** — sharded `RwLock` vs `dashmap` vs per-slot
  `arc-swap`/atomics. Start simple (sharded RwLock); go lock-free only if profiled.

---

## 12. Decided-against (so these aren't re-litigated after a context reset)

- **A second *gameplay* world inside a render world.** Bevy's render world is for GPU
  pipelining only — and `bevy_render` is off here anyway. Rate-decoupling comes from the
  sim/gameplay split + pools, not from putting gameplay in a render world.
- **Storing the voxel grid as World-mediated components with all access in-schedule.**
  That forces heavy work onto the frame-blocking schedule. Resolved by `Arc` CoW + the
  two-world-on-two-threads split (components are still fine for the *handle*).
- **A subscriber *list* per chunk.** A `u16` refcount suffices; a list is only needed to
  enumerate *who*, which unloading never requires.
- **An explicit multi-phase LOD load state machine.** Distance-priority over concentric
  shells gives staged expansion for free (§4).
- **Replicating voxel grids via per-tick component replication.** Terrain is locally
  generated and deterministic; only the edit overlay + entities sync (§1).
- **One-chunk-at-a-time gen requests.** The generator gets a prioritized *set* so it can
  batch by locality (§4).
- **Per-tick full scans of the desired set to check residency.** Delta-on-crossing only
  (§3).
