# voxel_engine Prototype — Technical Analysis

> Analysis of `voxel_engine` (the previous attempt), a single-process Bevy-ECS +
> `modul`/wgpu voxel demo. No networking, no persistence, single World. This
> document describes *methods, exact data layouts, and how subsystems connect*,
> not module structure — file:line references are for lookup only. Rendering
> features beyond basic drawing/paging/culling/LOD/AO (shadows, clustered
> lighting, atmosphere, TAA, WBOIT) are covered at the end and are secondary —
> `RENDERING_NOTES.md` already diagnoses the shadow subsystem specifically as
> the reason this prototype doesn't scale, and slates it for replacement.

## Voxel storage

`BlockId = u32`, `CHUNK_SIZE = 32` (`chunk/mod.rs:12-24`). Two-variant storage:

```rust
enum ChunkStorage {
    Filled(BlockId),                              // uniform chunk, O(1) memory
    Paletted { palette: Vec<BlockId>, data: Vec<u64>, bits_per_entry: u32 },
}
```

Palette width is variable: `bits_for_count` (`chunk/mod.rs:284-296`) picks 1/2/4/8/16
bits depending on distinct-block count (≤2/≤4/≤16/≤256/≤65536 types).
`from_flat_array`/`set` index voxels as `x + y*32 + z*1024` and pack that index's
palette slot into a flat `Vec<u64>` via `write_packed`/`read_packed`
(`chunk/mod.rs:303-329`): `bit_offset = index * bits`, `word = bit_offset/64`,
`bit = bit_offset%64`; if `bit + bits > 64` the value straddles into
`data[word+1]`, masked and shifted explicitly rather than padding to a byte
boundary — this is what makes 4-bit and 1-bit widths actually save memory
instead of rounding up to a byte per voxel. `set()` widens in place
(`widen_data` fully repacks the array into the new bit width) whenever a new
block type pushes the palette past the current width's capacity; it never
narrows back down after removals.

`ChunkData(Arc<ChunkStorage>)` is the component. Edits go through
`Arc::make_mut` (clone-on-write, `main.rs:685`) — async meshing/generation tasks
hold their own `Arc` clone and never observe a torn write, and a stale in-flight
mesh job is simply superseded by the next one rather than synchronized against.

Per-chunk occupancy has a second representation used only by the shadow
tracer: `ChunkBitmask` (`chunk/mod.rs:433-438`) — a 64-bit coarse mask (one bit
per 4×4×4 region) plus a 512×`u64` fine mask (one bit per voxel, 32×32×32 =
32768 bits). `build_bitmask` (`chunk/mod.rs:450-505`) sets a fine bit only for
*opaque* blocks (transparent blocks read as "air" for the fine mask, so
shadow rays pass through them by default) and a coarse bit for any non-air
region (opaque **or** transparent — the coarse mask can't distinguish, which
is why the shadow tracer needs the separate transparent-color indirection
described later). This is a pure function of `ChunkStorage`, entirely decoupled
from meshing.

## Terrain generation

`GenPool` (`chunk/generation.rs:36-96`) is a channel-based worker pool: `available_parallelism()/2`
threads (floor 1) pull `GenRequest`s off an unbounded `crossbeam_channel`, run
`generate_terrain`, push `GenResult` back over a second channel. `capacity()`
(`max_in_flight - in_flight.load()`, where `max_in_flight = num_threads*4`) is
the sole backpressure mechanism — `ChunkLoader` won't submit more requests
than that in one frame, regardless of how many chunks are pending.

`generate_terrain(pos, lod)` (`chunk/generation.rs:124-208`) samples two
independent FastNoiseLite Perlin-FBm fields at world coordinates
`wx = chunk_pos.x * 32 * 2^lod + x * 2^lod` (i.e. LOD>0 samples the *same*
continuous height field at coarser spacing, not a downsample of LOD 0's
output): 7 octaves at `scale=0.025`, remapped to `[0,1]` and raised to the 8th
power (flattens most terrain near 0, producing rare tall spikes) times
amplitude 40000, plus 16 octaves at `scale=0.5` times amplitude 200 (rolling
surface detail). Per-column height `= n1^8*40000 + n2*200`. Block selection is
a straight if-chain per voxel:

```
wy > height && wy <= lava_level → LAVA
wy > height                      → AIR
wy >= height                     → GRASS   (the surface voxel itself)
wy >= height - 3                 → DIRT
else                              → STONE
```

`lava_level = 200 - lod_scale` — a single fixed-height global "sea" that fills
any air pocket below that level, independent of terrain height; it exists
specifically to stress-test LTC area lights (lava is the engine's only
`LtcFromMesh` emitter — see meshing). A fast path computes the first voxel's
block type up front and tracks an `all_same` flag while filling the 32³ array;
if every voxel matched, `ChunkStorage::Filled` is returned directly instead of
building a palette — cheap for deep-underground or high-sky chunks.

## Chunk streaming & demand

`ChunkSource` (`chunk/demand.rs:10-34`, attached to the camera entity) holds
`start_radius`/`step`/`end_radius`/`lod_count` and a `last_camera_chunk` gate so
`update_chunk_demand` (`chunk/demand.rs:48-133`) is a no-op unless the camera
crossed into a new LOD-0 chunk this frame.

It builds `ChunkLoadList.segments: Vec<Vec<(IVec3, u8)>>` — concentric Chebyshev
shells (`max(|dx|,|dy|,|dz|)` radius test, so shells are cubes not spheres)
from `end_radius` down to 0, one segment per LOD per shell, appended
**outermost/lowest-priority first**. Segments are consumed back-to-front
later (`pending_generation.drain(start..)` pops from the end), so the
innermost, highest-detail region reaches the generator first. Each shell for
`lod+1 < lod_count` is expanded to whole 2×2×2 groups aligned to the parent
LOD's grid (`chunk/demand.rs:99-118`, via `pos.div_euclid(2)*2` then emitting
all 8 children) — this exists purely so the LOD coverage check in the render
pipeline (`is_fully_covered`) can flip from "not covered" to "covered"
atomically for a whole parent chunk instead of one child at a time, avoiding
visible flicker at LOD seams.

`ChunkLoader` (`chunk/loading.rs:15-28`) is the central lifecycle owner:
`loaded`/`in_flight`/`completed`/`desired` sets plus a `pending_generation`
priority queue, all keyed by `(IVec3, u8)`. `update_chunk_loading`
(`chunk/loading.rs:35-169`) runs every frame in five phases:

1. **Poll** `GenPool` results; for each, remove from `in_flight`, add to
   `completed`, and (if the entity still exists) `commands.insert(ChunkData(..))`
   and push a `ChunkChange{entity,pos,lod}` onto `ChunkChangedQueue`.
2. **Rebuild `desired`** — but only if `Query<&ChunkLoadList, Changed<ChunkLoadList>>`
   actually matched something this frame. This `Changed<>` filter is what
   turns a potential every-frame `HashSet` diff over tens of thousands of
   chunks into a true no-op on the common case (camera hasn't moved chunks).
3. **Unload**: chunks in `loaded` but not `desired` are despawned; each pushes
   a `ChunkUnload{entity,pos,lod}` onto `ChunkUnloadQueue` for render-side
   cleanup and is removed from `LodChunkMaps`/`LoadedChunkIndex`.
4. **Spawn**: newly-desired chunks get `commands.spawn((ChunkPos, ChunkLod))`,
   inserted into the right `LodChunkMaps` level and `loaded`. The
   `pending_generation` queue is then rebuilt from scratch from every
   `ChunkSource`'s segments in priority order — this interleaves multiple
   demand sources' segments so no single source starves another (round-robin
   fairness falls out of "rebuild the whole queue in segment order" rather
   than being tracked explicitly).
5. **Submit**: drain up to `GenPool::capacity()` entries off the back of
   `pending_generation` and hand them to the generator.

## Meshing — binary greedy meshing

Two logical stages (face extraction, then greedy merge) implemented as one
bitwise pass over padded Y-axis column bitmasks, not per-voxel loops.

**Columns** (`build_columns`, `chunk/meshing.rs:454-539`): for every `(x,z)` a
`u64` column has bit `y+1` set if that voxel is opaque (`opaque_cols`), plus one
such column array per distinct transparent block type present in the chunk's
palette (`trans_cols`). Bits 0 and 33 are padding, filled from the six
neighbor chunks (`fill_padding_col`) so face tests never need bounds checks —
if a neighbor isn't loaded, **opaque** padding is left as air (so a boundary
face stays visible rather than being wrongly culled against an unloaded
neighbor) but **transparent** padding is filled fully solid (so a transparent
boundary face is suppressed rather than drawn against nothing, avoiding
z-fighting with whatever loads in later). Grid is `34×34` (`CS_P = CS+2`) — the
padding trick turns "is this face visible" into one `u64` shift+AND-NOT per
32-voxel row instead of a per-voxel neighbor lookup.

**Face rows**: `build_lateral_face_rows`/`build_y_face_rows`
(`chunk/meshing.rs:545-593`) compute, per layer, `visible = (my_col >> 1) & !(neighbor_col >> 1) & VALID`
— one word op covers an entire row of 32 voxels at once.

**AO**: `compute_ao` (`chunk/meshing.rs:397-416`) samples, per quad corner, the
2 edge-adjacent voxels and 1 diagonal voxel via `is_solid_at` (which consults
neighbor chunks at chunk boundaries), giving the standard
`ao_val = 3 - (side_u + side_v + diag)` when the two edge neighbors aren't both
solid (if they are, `ao_val = 0` — a fully enclosed corner, regardless of the
diagonal). The 4 corner values (0-3, i.e. 2 bits each) are packed into one
`u8`: `ao_byte |= ao_val << (corner*2)`. This is computed once per visible
face position into an `ao_cache` before merging begins, and then used as a
**merge-blocking criterion identical to block type** — two adjacent faces only
merge (forward or lateral) if their `ao_byte`s are bit-for-bit equal. This is
why the packed byte is correct for the *enlarged* quad's corners too: since
merging never happens across an AO difference, every voxel absorbed into a
quad shares the exact same 4-corner AO, so reusing the anchor face's byte for
the whole merged quad is exact, not an approximation — the tradeoff is that
quads stop growing wherever AO varies even slightly, which is the real reason
big open areas mesh into large quads but AO-rich corners produce many small
ones. Emissive blocks (`is_ltc_emissive`) skip AO entirely (forced to 0)
specifically so lava-lake faces aren't fragmented by this rule — a whole lake
merges into one quad and one LTC light instead of dozens.

**Greedy merge** (`greedy_merge_layer`, `chunk/meshing.rs:686-784`): scans each
row's bitmask with `trailing_zeros`/`bits &= bits-1` to visit only set bits.
For the bit at `(fwd, bit_pos)`: first tries a **forward merge** (into
`fwd+1`) if the same bit is set there, the block type matches, and the AO
matches — if so it just increments a per-bit-position `forward_merged` run
length and continues without clearing the bit yet. Otherwise it does a
**lateral merge**, extending `width` across consecutive set bits in the same
row as long as their `forward_merged` count, AO, and block type all agree,
clears the consumed bits with a shifted mask, and emits one `FaceData` quad
whose `(w, h)` come from `(width, forward_merged+1)` remapped through
`face_wh` to the direction's actual U/V axes.

**Standard vs. border faces**: at the boundary layer of each direction (`layer
== 0` or `layer == CS-1` depending on direction), the mesher additionally
computes `border = my_block_bits & !visible_bits` — faces that exist
geometrically but are currently hidden by a same-LOD neighbor — and meshes
them into a *separate* `DirFaces.border` list alongside `.standard`. This
exists purely for LOD seams: the renderer only draws border faces on a
direction whose same-LOD neighbor is absent *and* not fully covered by finer
LOD (see LOD section). If a direction has zero standard faces its border list
is dropped outright (`meshing.rs:195-204`) — a buried chunk face is pure
overhead no matter what LOD state its neighbor is in.

**Transparent faces** are meshed once per distinct transparent type in the
chunk's palette, with the "hides" test being `opaque | same_type` — glass next
to water still draws a face, glass next to glass doesn't, and neither type
gets fully culled by the other.

**Light extraction is piggybacked on the same pass**, not a separate mesh
traversal:
- *LTC lights*: `greedy_merge_layer` emits a `ChunkLocalLtcLight` (quad
  corner position + `edge_u`/`edge_v` vectors + normal + bounding-sphere
  radius, all still in chunk-local voxel space) alongside every merged quad
  whose source block has `LightSpec::LtcFromMesh` — no extra scan, same
  iteration that already produces the geometry.
- *Simple lights* (`extract_simple_lights`, `chunk/meshing.rs:97-162`): a
  separate linear voxel scan, but with two fast paths — a `Filled` chunk does
  one `block_props` lookup and, if that block has no light spec, returns
  immediately without ever iterating 32,768 positions; a `Paletted` chunk
  first checks whether *any* palette entry has a light spec and aborts early
  if not.

`GenPool` and `MeshPool` (`chunk/meshing.rs:164-227`) share the identical
channel-worker-pool shape (see cross-cutting patterns). `resolve_changes`
(`chunk/meshing.rs:241-262`) marks `NeedsRemesh` (a `SparseSet` component,
chosen so its per-frame add/remove churn doesn't move entities between
archetypes) on the changed chunk plus any of its 6 neighbors that already have
`ChunkData` — a boundary edit needs the neighbor's face culling redone too.
`start_meshing` clones `Arc<ChunkStorage>` for the chunk and up to 6 neighbors,
dispatches a `MeshRequest`, and **removes `NeedsRemesh` immediately** (not on
completion) — so there's no marker preventing a second edit from dispatching a
second overlapping mesh job for the same chunk before the first one lands.
`poll_meshing` applies whichever result arrives via a plain component insert
(full replace, not merge), so an out-of-order completion is self-correcting
within a frame or two rather than actively prevented. An all-air chunk
short-circuits with empty results without touching the worker pool at all.

## Chunk lifecycle & system wiring

The subsystems above are connected entirely through Bevy components/events and
an explicit schedule dependency graph — there's no direct function-call
coupling between generation, meshing, and rendering.

**Component transitions for one chunk entity**, in order:

| Stage | System | Adds | Removes |
|---|---|---|---|
| spawn | `loading::update_chunk_loading` (phase 4) | `ChunkPos`, `ChunkLod` | — |
| generated | `loading::update_chunk_loading` (phase 1) | `ChunkData` | — |
| invalidated | `meshing::resolve_changes` | `NeedsRemesh` (SparseSet) | — |
| mesh dispatched | `meshing::start_meshing` | — | `NeedsRemesh` |
| mesh landed | `meshing::poll_meshing` | `ChunkFaces`, `TransparentChunkFaces`, `ChunkSimpleLights`, `ChunkLtcLights` | — |
| GPU uploaded | `render::synchronize_gpu` | (GPU pages + `ChunkRenderData` entry) | `ChunkFaces`, `TransparentChunkFaces` |
| lights uploaded | `render::lights::synchronize_lights` | (baked into `ChunkLightStore`) | `ChunkSimpleLights`, `ChunkLtcLights` |
| unload | `loading::update_chunk_loading` (phase 3) | pushes to `ChunkUnloadQueue` | despawns entity |
| cleanup | `render::cleanup_unloaded_chunks` | — | GPU pages, shadow grid entry, transparent color entry |

`ChunkFaces`/`TransparentChunkFaces`/`ChunkSimpleLights`/`ChunkLtcLights` are
all transient: produced by the mesh worker, consumed and immediately removed
by exactly one downstream system apiece, never read a second time.

**`ChunkChangedQueue` fan-out**: generation completing (phase 1 above) and
manual block edits (`main.rs:663-693`, on right/left mouse click) are the only
two writers of `ChunkChangedQueue`. Three independent systems read it the same
frame without knowing about each other: `meshing::resolve_changes` (Redraw
schedule, drives remeshing), `shadow::grid::process_chunk_bitmasks` and
`shadow::grid::process_chunk_transparent_colors` (Synchronize schedule, drive
the shadow acceleration structure). `chunk::clear_chunk_changed_queue` runs
last, ordered `.after()` both shadow-grid readers, and empties the queue for
next frame.

**Redraw schedule** (`main.rs:245-263`) is one explicit chain:
`process_input → update_chunk_demand → update_chunk_loading → ApplyDeferred
→ resolve_changes → ApplyDeferred → poll_meshing → start_meshing`, all
`.before(RenderSystemSet)`. Note `poll_meshing` runs *before* `start_meshing`
in the same frame — this frame's poll drains jobs dispatched in an earlier
frame, and jobs dispatched by this frame's `start_meshing` are earliest polled
next frame; meshing always has at least one frame of latency by construction,
never zero.

**Synchronize schedule** (`main.rs:266-289`) is a partial-order constraint
graph, not one chain — the load-bearing edges are `update_camera →
synchronize_lights → cleanup_unloaded_chunks → synchronize_gpu` (lights must
read `ChunkUnloadQueue` before cleanup drains it, and must use this frame's
camera chunk offset for the dynamic-light rebase) and `process_chunk_bitmasks
/ process_chunk_transparent_colors / update_shadow_grid_origins →
synchronize_shadow_buffers` (CPU-side grid/pool edits must land before the
GPU upload pass reads them). Everything else in that set is free to run in
whatever order the scheduler picks.

**GPU operation sequence** (`init_window`, `main.rs:325-343`) is the actual
per-frame command-encoder order, separate from the ECS schedule above: `ClearAll
→ ShadowDepthOperation (depth+normal prepass) → ShadowTraceOperation (compute)
→ ClusterBuildOperation (compute) → TaaVoxelDrawOperation (opaque geometry) →
SkyPassOperation → TransparentDrawOperation (WBOIT accumulate) →
WboitResolveOperation (composite) → TaaResolveOperation (final composite +
history) → ShadowDebugOverlay (optional)`. Every later pass either reads a
buffer/texture a prior pass wrote (shadow mask, cluster index list, WBOIT
accumulation) or shares the surface's depth buffer (sky pass and transparent
pass both read-only against the opaque depth written by the voxel draw).

## LOD system

`lod_count` levels (default 8), scale `2^lod`, one `ChunkMap` per level inside
`LodChunkMaps`. LOD>0 terrain is generated directly (not downsampled from LOD 0)
at coarser voxel spacing — see generation above. Per the project's own
`SERVER_CLIENT_ARCHITECTURE.md` design note (echoed here since it governs
`resolve_changes`): LOD>0 chunks are never re-meshed on block edits — edits are
only ever pushed to LOD 0, so higher LODs silently diverge from live edits at
a distance where it isn't visible.

**Coverage/visibility**: `is_fully_covered(pos, lod, &LoadedChunkIndex)`
(`render/mod.rs:694-710`) checks whether all 8 child chunks at `lod-1` are
present in a `HashSet<(IVec3,u8)>` of uploaded (pos, lod) pairs. If so, the
parent chunk is dropped from the draw cache entirely (`build_draws_for_entry`,
`render/mod.rs:914-934`) — finer LOD fully replaces it, no overdraw. This is
also where the demand system's 2×2×2-aligned segment expansion pays off: a
shell's children all become "loaded" in the same demand pass, so coverage
flips in one step instead of chunk-by-chunk flicker.

**Border faces meet coverage**: `build_draw_for_direction`
(`render/mod.rs:836-908`) checks whether the neighbor in a given direction is
`is_fully_covered`; if yes, the draw includes `border` faces too (fills any
gap at the seam where the neighbor's finer LOD doesn't perfectly align);
otherwise only `standard` faces are drawn. When a chunk *becomes* fully
covered, its neighbors' cached draws in the opposite direction are
specifically rebuilt to pick up/drop border faces (`render/mod.rs:1144-1182`)
rather than waiting for a full cache rebuild.

**Draw ordering**: LOD 0 draws first in the indirect buffer, then LOD 1, etc.
(`write_draws_to_indirect`, `render/mod.rs:1253-1282`) — with reverse-Z depth
test, LOD 0 geometry writes depth first so any coarser, larger geometry behind
it at a seam gets depth-rejected instead of overdrawn.

## Rendering pipeline — memory layout, culling, draw submission

### Face data & page metadata: exact wire format

`FaceData` is 8 bytes, `#[repr(C)]`, uploaded verbatim as vertex-buffer bytes
(`chunk/mod.rs:166-175`):

| Field | Type | Byte offset | Meaning |
|---|---|---|---|
| `x` | `u8` | 0 | quad anchor, chunk-local |
| `y` | `u8` | 1 | quad anchor, chunk-local |
| `z` | `u8` | 2 | quad anchor, chunk-local |
| `w` | `u8` | 3 | greedy-merge width along the direction's U axis |
| `h` | `u8` | 4 | greedy-merge height along the direction's V axis |
| `material[0]` | `u8` | 5 | packed AO: 4 corners × 2 bits |
| `material[1]` | `u8` | 6 | `BlockId as u8` — **truncated from the `u32` chunk storage type**, so only block IDs 0-255 survive into the mesh |
| `material[2]` | `u8` | 7 | unused (always 0 today; reserved) |

The vertex buffer layout (`render/mod.rs:671-686`) reads this as two `Uint8x4`
attributes at stride 8: `data0 = (x,y,z,w)` at offset 0, `data1 = (h, mat0,
mat1, mat2)` at offset 4 — i.e. `data1.y` is the AO byte and `data1.z` is the
block id, as consumed in `voxel_vertex.wgsl`.

`PageMetadata` is 16 bytes, one entry per **page** (96 faces) in a parallel
storage buffer, written once at upload time and never touched again until the
page is deallocated:

| Field | Type | Notes |
|---|---|---|
| `chunk_x/y/z` | `i32` each | integer chunk position, LOD-0 scale not applied here |
| `direction_and_lod` | `u32` | bits 0-7 = direction (0-5), bits 8-15 = LOD level |

`DrawIndirectArgs` (16 bytes, standard wgpu indirect layout) is rebuilt every
frame per surviving cached draw: `vertex_count=6, instance_count=<face count in
this page's contribution>, first_vertex=0, first_instance=page_index*PAGE_SIZE`.
The vertex shader recovers which page an instance belongs to via
`page_index = instance_index / PAGE_SIZE` and indexes into the metadata
storage buffer with it — this is the only link between an instance and its
chunk position/direction/LOD; nothing is duplicated per-vertex.

### AO, traced end to end

This is a representative trace of how a single CPU-computed value crosses the
whole pipeline into a lit pixel:

1. **CPU, meshing**: `compute_ao` (`chunk/meshing.rs:397-416`) computes 4
   corner AO values (0-3 each) for a quad and packs them into one byte,
   `material[0]`, as part of the `FaceData` emitted by `greedy_merge_layer`.
2. **CPU, upload**: `upload_direction_faces` (`render/mod.rs:712-762`) copies
   the `FaceData` slice byte-for-byte into a slab's face buffer via
   `queue.write_buffer` — no repacking, the CPU struct layout *is* the GPU
   layout.
3. **GPU, vertex shader** (`voxel_vertex.wgsl:60-92`): `data1.y` is unpacked
   back into 4 floats (`ao_00..ao_11`, each `/3.0` to normalize to `[0,1]`).
   The shader also flips which diagonal splits the quad's two triangles
   whenever AO is asymmetric (`ao_00+ao_11 < ao_10+ao_01`) — otherwise linear
   interpolation across the "wrong" diagonal produces a visible seam.
4. Each of the 6 vertices bilinearly samples its corner via
   `out.ao = mix(mix(ao_00,ao_10,u), mix(ao_01,ao_11,u), v)` and writes it to
   a plain (smoothly interpolated, not `@interpolate(flat)`) varying.
5. **GPU, rasterizer**: interpolates `ao` per-fragment across the triangle for
   free.
6. **GPU, fragment shader**: `evaluate_material` forwards `in.ao` into a
   `Surface`; `apply_lighting` (`lighting.wgsl:66-68`) computes
   `ao_term = mix(0.4, 1.0, surface.ao)` and multiplies both the constant
   ambient term and the sky-light term by it before adding sun diffuse and
   fog.

No AO recomputation ever happens on the GPU — the compute-heavy neighbor
sampling is 100% CPU-side and amortized across however many frames a chunk
stays meshed the same way; the GPU only interpolates and applies a scalar.

### Paged GPU layout and allocation

A *page* is `PAGE_SIZE=96` faces; a *slab* (`render/mod.rs:212-267`) is
`PAGES_PER_SLAB≈175k` pages (~128MB) holding a face vertex buffer plus the
`PageMetadata` storage buffer above. `PageAllocator` (`render/mod.rs:298-333`)
is a thin free-list-per-slab allocator: it scans existing slabs for a free
page, and only allocates a new 128MB slab when every existing one is full —
slabs are never freed, only their internal pages are, so the process's GPU
memory floor only ever grows in 128MB steps and never shrinks for the
lifetime of the run.

**How pages map to chunks**: a page never spans two chunks or mixes two
directions — it's the unit of upload, not a spatial partition. Each uploaded
chunk gets one `ChunkRenderEntry` (`render/mod.rs:347-352`) holding
`directions: [DirectionPages; 6]` (opaque) and `transparent_directions: [DirectionPages; 6]`.
`upload_direction_faces` (`render/mod.rs:712-762`) is called once per
`(chunk, direction)`: it concatenates that direction's `standard` faces
followed by its `border` faces into one `Vec<FaceData>`, splits it into
`PAGE_SIZE`-sized chunks via `.chunks(96)`, and allocates one page per chunk —
so a direction with ≤96 faces (the common case for a mostly-buried or mostly-flat
face) fits in a single page, while a densely-meshed direction spans several,
all recorded in that direction's `DirectionPages { pages: Vec<AllocatedPage>,
standard_faces, total_faces }`.

**Border faces usually sit in the buffer unread**: because standard and
border faces are uploaded as one concatenated stream, border faces physically
live in whatever page(s) follow the standard faces — often the tail of the
same last page. `build_draw_for_direction` (`render/mod.rs:836-908`) doesn't
store them separately; it walks `dir_pages.pages` front-to-back, capping the
cumulative instance count at `face_limit` (`standard_faces` normally, or the
larger `total_faces` only when the same-LOD neighbor exists and isn't fully
covered by finer LOD — see LOD section) via `page.face_count.min(faces_remaining)`,
and simply stops emitting `DrawIndirectArgs` once that cap is reached. Any
page — or the unread tail of a partially-included page — past the cap is
never referenced by an indirect arg that frame. So in the common case (a
same-LOD neighbor is loaded) every chunk carries some border faces resident
in GPU memory that no draw call ever touches; the cost is a little wasted VRAM
in trailing/partial pages rather than a second buffer or any extra
bookkeeping to keep border faces separately addressable.

### Draw cache and its two invalidation axes

`DrawCache`/`CachedDraw` (`render/mod.rs:354-397`): draw args for a `(chunk,
direction)` pair are built once and cached; per-frame work is limited to
frustum-culling the cache and copying surviving args into the indirect
buffer — not rebuilding anything. Invalidation is tracked along two
*independent* counters so a camera move doesn't force a structural rebuild and
vice versa:

- **Camera chunk crossing** (`cam_changed`, compares `last_camera_chunk`) only
  recomputes each entry's cached `backface_culled` flag
  (`compute_backface`, `render/mod.rs:807-831` — a plane test of the camera
  world position against the chunk's 6 AABB faces, done once per *direction*
  per chunk, not per triangle).
- **Generation bump** (`gen_changed`, `cache.generation` vs `cache.cached_generation`,
  incremented whenever any chunk is uploaded or unloaded this frame) drives a
  structural edit: entries for uploaded/removed entities are dropped,
  `newly_covered` parents (LOD coverage flipped on) are dropped, entries for
  `newly_uncovered` parents (a child unloaded, parent needs to draw again) are
  added back, and neighbors of newly-covered chunks get their opposite-facing
  border-face draws rebuilt in place rather than waiting for a full pass.

A second, structurally identical `TransparentDrawCache` exists for the
transparent pass, built with `do_backface_cull = false` (transparent faces are
visible from both sides, so backface culling would be wrong).

### Culling and draw submission

**Frustum culling** (`frustum_cull_cache`, `render/mod.rs:1219-1251`) runs
every frame regardless of camera-chunk state — a 6-plane AABB test
(`extract_frustum_planes`/`is_aabb_in_frustum`, `camera.rs:349-392`, planes
derived directly from the view-proj matrix's rows/columns, no separate
frustum object). Surviving args are grouped by `(slab, lod)` and written into
one flat indirect buffer (`write_draws_to_indirect`, LOD-0-first ordering —
see LOD section); the actual draw (`draw_voxel_geometry`, `render/mod.rs:1339-1350`)
issues one `multi_draw_indirect` per `(slab, lod)` group, only rebinding the
vertex buffer/metadata bind group when the slab changes — a chunk crossing
into a new slab is the only thing that causes a bind-group switch mid-draw.

### Vertex expansion and camera-relative math

**Vertex expansion** (`shaders/voxel_vertex.wgsl`): no index buffer — 6
vertices per instance. `direction` (unpacked from page metadata) selects an
`(offset, tangent_u, tangent_v)` basis from a hardcoded per-direction table;
quad corner = `pos + offset + tangent_u*u*w + tangent_v*v*h`, scaled by
`lod_scale = 1 << lod`. Chunk origin is computed entirely in integer space
before any float touches it: `rel_chunk = chunk_pos*lod_scale -
camera.chunk_offset`, then `chunk_origin = f32(rel_chunk * CHUNK_SIZE)`.

**Camera-relative math**: `Position` is `f64` world-space, but the GPU only
ever receives a small `f32` local offset within the *current* chunk plus a
separately-uploaded `i32` chunk offset (`camera.rs:130-155`,
`CameraUniform.chunk_offset`). Every shader that needs a world position
(vertex expansion, lights, shadow tracer) rebases `chunk_pos - camera.chunk_offset`
in integer space first — this pattern is applied uniformly to geometry,
lights, and shadow rays, not just the main camera, and is the only reason f32
precision doesn't degrade at large world distances.

`CameraUniform` (`camera.rs:159-173`, one buffer, bound as group 0 nearly
everywhere) is the single most shared piece of GPU state: `view_proj`,
`chunk_offset`, `screen_size`, `jitter_offset` (TAA), `inv_view_proj` (depth
reconstruction, used by both the shadow tracer and TAA resolve),
`prev_jittered_view_proj`/`prev_chunk_offset` (temporal reprojection, shared
by TAA and the shadow tracer's reprojection blend), `frame_index` (drives both
TAA jitter and the shadow tracer's checkerboard/jitter pattern), and
`camera_local_pos`.

### Shading

`shaders/voxel.wgsl` triplanar-projects world position onto 2D per face
direction, computes `textureSampleGrad` derivatives **before** the UV
`fract()` wrap (so mip selection doesn't break at tile boundaries), and looks
up a fixed atlas quadrant per `(material_id, direction)` (grass gets a
different quadrant on top vs. sides vs. bottom/dirt). `apply_lighting`
(`lighting.wgsl`) combines: sun `N·L` gated by a 3×3 edge-aware upscale of the
shadow mask (rejects neighbor texels whose encoded normal or reconstructed
height diverges from the current fragment, falling back to the nearest valid
sample if none qualify); `ao_term = mix(0.4,1.0,ao)` multiplying both the
constant ambient term and the sky-light term; additive clustered-light
contribution; exponential fog last. Lava is special-cased in `fs_main` to skip
the lit path entirely (an emissive surface shaded as a Lambert receiver would
read as near-black) and just fog-composite a fixed hot color.

## Camera & editing

`Camera` (`camera.rs`) hand-rolls view/projection (no glam camera helpers),
using a reversed-Z projection (near→1, far→0, and the projection matrix drops
the far plane from the math entirely — `perspective()` doesn't use its `_far`
parameter) — `CompareFunction::GreaterEqual` depth tests are used throughout
the renderer to match. `FlyCamera` is a yaw/pitch/WASD+mouse controller with
scroll-to-adjust-speed and a Ctrl sprint multiplier. TAA jitter uses
Halton(2,3) sub-pixel offsets (`taa_jitter`, `camera.rs:279-282`) applied
directly to the projection matrix's translation terms (`apply_jitter`), not as
a separate shader uniform consumed downstream.

Block editing (`main.rs:663-693`) uses its own independent DDA voxel raycast
(`raycast_blocks`, `main.rs:397-470`, LOD-0 only, walks `ChunkData` directly —
unrelated to the shadow system's DDA). On hit, `Arc::make_mut` mutates the
chunk in place (palette widens automatically if needed) and pushes a
`ChunkChange` event consumed next frame by `resolve_changes`, re-entering the
same lifecycle a freshly generated chunk goes through.

---

## Beyond basic drawing (secondary — see `RENDERING_NOTES.md` for the project's take)

### Shadows — compute ray-traced soft shadows

A `ShadowGrid` (`render/shadow/grid.rs`) is a wrapping-indexed 3D array per LOD
level, sized to `end_radius`, storing either `GRID_EMPTY`, `GRID_SOLID`, or a
slot index into a pooled `BitmaskPool` (the per-chunk two-level bitmask from
the storage section). On a camera chunk crossing, `update_origins` does an
**incremental** update — only the newly-exposed edge slices are cleared and
refilled from source `HashMap`s using modular ("wrapped") indexing, so
existing grid data never moves in memory; a full rebuild only happens on a
jump larger than the grid itself (teleport/debug freeze). A parallel
per-region transparent-color indirection (4×4×4 `u32` slots per chunk,
pointing into a `TransparentColorPool` of 8×8×8-byte regions, byte values
0-253=color index / 254=air / 255=opaque) piggybacks on the same grid so the
tracer can distinguish "no transparent blocks here, use the fine bitmask" from
"use the per-voxel color buffer instead" with one indirection read per
8-voxel region.

The compute shader (`shaders/shadow.wgsl`) reconstructs world position from a
downscaled depth+normal prepass, then DDA-marches toward the sun: first
hopping between chunks via the LOD grid (escalating to a coarser LOD when the
ray exits the current grid's radius), then within a chunk's bitmask using the
coarse 4×4×4 mask to skip empty 8-voxel regions in O(1) (jump straight to the
region's exit face) before falling back to the fine per-voxel bitmask. It
checkerboards (traces half the pixels per frame, reprojecting the other half
from the previous frame's accumulated result) and temporally accumulates with
a reprojection blend rate that adapts to camera motion and per-pixel history
divergence. Transparent voxels accumulate Beer-Lambert absorption
(`-ln(tint)` coefficients, precomputed on CPU into a 254-entry uniform array)
along the ray instead of terminating it, producing colored shadows;
`trace_result = exp(-total_absorption)` at the end of the walk.

### Clustered lighting

Two independent light lifecycles: **chunk-owned** lights (LTC area lights from
merged emissive quads, simple point/spot lights from a block scan) are
extracted during meshing, stored GPU-resident, and only re-uploaded when the
set of loaded chunks changes (`ChunkLightStore`, dirty-flag gated rebuild of a
flat buffer); **ECS-owned dynamic** lights (e.g. the debug camera headlamp)
are re-uploaded from a flat ECS query every frame, capped at 1024. A
Doom-2016-style cluster grid (16×9 screen tiles × 24 log-depth slices, 3456
clusters total) is rebuilt every frame by a compute pass that atomically
appends `(buffer_id<<28 | index)`-encoded light indices into per-cluster
slices of a flat index list (`MAX_LIGHTS_PER_CLUSTER=256` per cluster,
overflow silently dropped); the fragment shader looks up its cluster from
clip-space position and walks only that slice. LTC evaluation
(`shaders/lights_common.wgsl:35-111`) turned out not to need the LUTs the
design doc originally called for — since voxel surfaces are Lambert, it
collapses to the analytic Hill-Reed polygonal cosine integral (`ltc_integrate_edge`,
a rational minimax fit of `theta/sin(theta)`), evaluated directly per light
per fragment with no texture samples at all, plus a coplanar-epsilon guard
specifically to stop the lava sea from self-illuminating (a surface exactly
on the emissive quad's own plane would otherwise flicker from near-degenerate
cross products).

### Atmosphere / sky

CPU-side Rayleigh+Mie single-scattering integration (`render/atmosphere.rs`)
bakes a 3D LUT (64×64 texels, 64 slices — one per sun angle around the day
cycle, so the day/night cycle is "free" at runtime, just an LUT slice lookup)
at startup, stored as `f16` (hand-rolled `f32_to_f16` bit-twiddling, no
external half-float crate). A second LUT is baked with isotropic Mie (`g=0`,
vs. `g=0.76` for the sky-glow LUT's forward scattering) specifically for fog
tinting — same integration code, different asymmetry parameter, baked once
each and both sampled at runtime.

### TAA

Standard history ping-pong with Halton jitter (shared with the shadow
tracer's jitter, both driven by `frame_index`); renders voxels to an offscreen
scene texture rather than directly to the surface so the resolve pass can
blend it against reprojected history before the final composite MRT write
(surface + next frame's history, one pass).

### WBOIT (transparency)

Two-target weighted-blended order-independent transparency: an `Rgba16Float`
accumulation target (additive blend: `color.rgb*alpha*weight, alpha*weight`)
and an `R8Unorm` revealage target (multiplicative `dst*(1-src)` blend), both
depth-tested (but not depth-written) against the opaque geometry's depth
buffer, resolved with a single fullscreen pass: `final = (1-revealage)*opaque
+ accum.rgb/max(accum.a,0.001)`.

---

## Cross-cutting patterns worth naming

- **Identical worker-pool shape** for generation and meshing: unbounded
  crossbeam channels, `available_parallelism()/2` threads, `Drop` closes the
  request-channel sender so workers' blocking `recv()` returns `Err` and they
  exit cleanly on join — no explicit shutdown signal needed.
- **Two independent invalidation axes** recur beyond the draw cache — the
  shadow grid separates "camera moved" (origin shift, incremental slice
  refill) from "chunk data changed" (dirty-slot re-upload); the demand system
  separates "camera crossed a chunk" (rebuild segments) from "priority list
  changed" (`Changed<ChunkLoadList>` gate on the loader).
- **Arc + copy-on-write** is the only synchronization primitive for chunk
  data; there are no locks anywhere in the hot path, and staleness is handled
  by "the next update wins" rather than by coordination.
- **`chunk_pos: i32 + local_pos: f32`** camera-relative encoding is applied
  uniformly to geometry, chunk-owned lights, dynamic lights, and shadow rays —
  every GPU-facing position follows the same two-part rebase.
- **Transient marker/data components** (`NeedsGeneration`-equivalent state via
  `ChunkData` presence, `NeedsRemesh`, `ChunkFaces`, `ChunkSimpleLights`, ...)
  are the whole inter-system contract — no subsystem calls another's code
  directly, they only add/remove components and read each other's queues.
