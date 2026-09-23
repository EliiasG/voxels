# Rendering Design Notes

> Design discussion captured 2026-06-14. Target: Rust + wgpu voxel engine,
> 3D chunk map, sub-Minecraft voxel size (~1/3–1/4), LOD with varying block
> sizes, large render distance, must run on low-end integrated GPUs.

## Guiding thesis

**Lighting is the cheapest artist you can hire.** Good lighting (GI, AO, soft
shadows, PBR response) makes simple geometry look great and de-Minecrafts the
look. Investing in the renderer *buys back* art effort. Teardown = colored
voxels carried entirely by lighting; Minecraft RTX = the same 16×16 art
transformed by light transport. So: lean into the renderer.

## Platform constraints that shape every decision

- **Rust → wgpu.** No reliable hardware ray tracing; the low-end iGPU floor has
  **no RT cores at all**.
- Therefore "ray tracing" = **software DDA marching through our own voxel grid
  in a compute shader**. The voxel grid *is* the acceleration structure (DDA +
  LOD mips let rays skip empty/distant space). No BVH needed.
- iGPUs are killed by **incoherent memory access** (weak bandwidth, tiny caches
  shared with CPU). Any per-pixel pointer-chasing into big storage buffers is
  the worst-case workload.

## Why the old prototype's per-pixel DDA sun shadow died

Diagnosis from `voxel_engine/src/render/shaders/shadow.wgsl`:

1. **Per-pixel secondary ray march.** One sun ray per pixel (checkerboarded to
   ½), each up to 512 chunk-steps × ~96 inner DDA steps, every step a dependent
   incoherent storage load. Cost is bolted to screen resolution → forced ugly
   downscaling, which temporal accumulation only papers over.
2. **The sun is a parallel light traced the expensive way.** Every ray shares a
   direction — that coherence is exactly what a shadow map captures for free in
   one rasterization pass. Tracing per-pixel throws it away and pays incoherent
   memory prices; neighbouring rays diverge → warp divergence on top.
3. **Grazing angles detonate step count.** Low sun (sunrise/sunset) = rays skim
   surfaces for hundreds of steps. Worst frame = dawn over open terrain.
4. **Hidden CPU/memory tax.** The whole shadow-grid + bitmask pool + transparent
   indirection + origin-wrap sync machinery exists only to feed this march, and
   re-syncs on every chunk change and camera-chunk crossing.

## Decision: stop tracing the sun per-pixel → Cascaded Shadow Maps (CSM)

For one directional light, **CSM rendered from the LOD meshes** is the right
tool, and this engine is unusually suited to it:

- Near cascade = LOD0; **far cascades = coarse LODs** → huge render distance is
  nearly free (you can't perceive crisp shadow detail at 2 km anyway).
- 1–2 orders of magnitude cheaper than per-pixel marching; runs on everything.
- **Deletes a whole subsystem** (shadow grid, bitmask pool, indirection, color
  pool, origin-wrap sync) — reuse meshes you already maintain.
- **Destructibility gets easier**: edit a voxel → you already re-mesh → next
  cascade render picks it up. No acceleration structure to keep coherent.
- Voxel worlds are a **best case** for shadow maps: axis-aligned faces, big flat
  greedy quads, easy slope-scaled bias (you know the 6 normals exactly).

What you lose vs the march, and the fixes:
- **Crisp contact**: a bounded 8–16-step screen-space contact-shadow march to
  clean the bias gap (fixed cost, not 512 steps).
- **Colored transparent (stained-glass/water) shadows**: the one genuinely nice
  thing only the march gave. If wanted, do it as a *sparse separate pass*, never
  in the main per-pixel hot loop.

### Cascade design: two independent axes — don't fuse them

- **Cascade** = which shadow map a *receiver* pixel reads, chosen by camera
  distance. A **resolution** decision.
- **Caster LOD** = how detailed the geometry rasterized into a map is. A
  **triangle-budget** decision.

The trap ("CSM per LOD" naively = cascade *i* renders LOD *i* uniformly) is what
creates the blocky-distant-tree-shadow fear. Fix: render each cascade from the
**active-LOD field** (finest loaded per region, chosen by camera distance — the
same field the main view uses). Then **one-LOD-per-region** holds everywhere →
the near tree is fine in every cascade that reaches it, the distant tree is a
coarse block only where it's actually far. The blocky-near-shadow problem solves
itself: near pixels read the near cascade.

### Cascade-per-LOD + carving (validated design)

- Cascade *i* renders LOD *i* with finer-LOD regions **carved out** (a hole).
- Each region is therefore rendered into **exactly one** cascade → no
  double-representation → **combining by logical OR of per-cascade occlusion is
  safe** (can't over-darken). Shadowed = any cascade reports an occluder.
- Flat ground: LOD1 has a hole at the player's column → reports "lit" → defers to
  LOD0's crisp result. ✓
- Far caster not in LOD0 (hill/cliff beyond LOD0 range): no hole there in LOD1 →
  rasterized → OR picks it up → shadow casts. ✓

#### CRITICAL correction: carve **geometry**, not **footprint**

The "hole" must be a hole in *rasterized geometry only*. Each cascade's
shadow-map **footprint stays concentric and covers the near columns**, including
the player's, even though that cascade's geometry lives in a far shell. Reason:
the point on a cave ceiling that shadows the player sits on the player's sun-ray
→ same light-space XY as the player. For LOD1's map to capture it, LOD1's
footprint must include the player's column. Shrinking the footprint to "just the
LOD1 shell" makes the cave go lit. So: nested concentric footprints, carved
geometry, empty regions read as lit-from-this-LOD and the finer cascade fills via
the OR.

**Bonus of tying cascade range to LOD range:** texel size auto-matches feature
size (LOD0 = fine geo + fine texels; LOD3 = coarse geo + coarse texels). Watch
out only if one LOD spans a huge depth range (may want an extra split inside it).

## The cave / far-occluder → near-receiver problem

A cave ceiling 200 m up, or a distant mountain whose shadow lands at your feet,
is a **far/large occluder shadowing a near receiver** — the one thing CSM is
structurally bad at (a small near cascade can't see far, laterally-offset
occluders). Two valid solutions:

1. **Cascade-per-LOD + carving + OR** (above): the far cascade contains the
   ceiling; the near pixel ORs against it. Unified, one code path.
2. **Hybrid fallback — coarse-only sun-occlusion march:** a separate
   **coarse-LOD-only (LOD2+), low-resolution, temporally-accumulated** march for
   far/large occluders, combined via `min(visibility)`. Cheap because coarse +
   low-res + low-frequency (a dark cave needs no crisp edge). **Doubles as the
   GI sun-visibility query.** This is the thing that fixes the original perf sin:
   the only marching left is coarse + low-res, the opposite of per-pixel
   full-detail.

Use the unified cascade design first; fall back to the coarse march for the
far-occlusion term if per-pixel N-cascade OR measures too hot.

Costs to plan for:
- **Per-pixel N-cascade sampling + OR.** Optimize: resolve the *coarse* cascades'
  union at **half/quarter res** (far-occluder shadows are low-frequency); sample
  only 1–2 fine cascades at full res. (This is "blend higher LODs onto lower" and
  it converges back to the coarse-march idea.)
- **Carving must be gap-free at the streaming frontier** or you get light leaks:
  carve a LOD*i* chunk only when *all* finer chunks covering its volume are
  loaded. Chunk-aligned carving keeps seams clean.
- **Bias grows with LOD** (coarse cascades have big texels → peter-panning).
  Fine for cave ceilings / distant cliffs (soft is desirable); don't reuse LOD0's
  bias on LOD3.

## Update scheduling: pull, not push

- **Pull model**: level *N*, on its own update cadence, reads whatever
  `merged[N+1]` currently holds (however stale) to build
  `merged[N] = OR(own[N] carved, merged[N+1])`. Never the reverse — a coarse
  level's update never notifies or forces work on finer levels. Matches "finer
  levels update far more often anyway": pushing from a rare coarse-update event
  would just be re-derived moments later by the finer level's own next cycle.
- **Dirty trigger: snap, don't schedule by frame-fraction.** Snap each cascade's
  footprint center to multiples of its own texel size; a cascade is dirty only
  when camera motion (or sun-angle drift) crosses a snap boundary. Update
  frequency falls out of actual movement instead of a hand-picked "LOD0 every 2
  frames, LOD1 every 4…" schedule, and needs no special-casing for teleport/fast
  travel — that's just many snap cells crossed in one frame, handled as a burst
  (render all now-dirty `own[N]` passes — independent of each other — then run
  the coarsest-to-finest merge reduction once).
- **Cap re-renders at ~1 cascade per frame via a dirty-priority queue**
  (finest/most-important dirty cascade drains first). Bounds per-frame GPU cost
  to something predictable without an async-compute queue (unreliable win on
  iGPU — real fencing/sync complexity for uncertain parallel throughput on
  exactly the hardware floor this project targets). A multi-cascade dirty burst
  just becomes a short backlog drained over the next few frames — free, since
  tolerating coarse-cascade staleness is the whole premise of this design.
- **LOD0 dynamic entities**: split into a cached static-terrain layer (dirtied
  by the snap-grid rule + voxel edits) and a small dynamic-entity overlay
  re-rendered every frame, composited at sample time. Same caching idea as the
  hero-light shadow maps above, applied to the sun's near cascade — avoids
  re-rendering all of LOD0's static geometry every frame just because one
  entity moved.
- **No reprojection needed for cross-cascade sampling.** Combining `own[N]`
  with a possibly-stale `merged[N+1]` (different last-render origin) is just a
  per-sample coordinate transform through each cascade's own stored light-space
  matrix — exactly how ordinary multi-cascade shadow sampling already works,
  since a directional-light buffer of static-ish geometry has no
  disocclusion/ghosting failure mode the way screen-space TAA does. True
  reprojection (an integer-texel shift) is only needed for the *optional*
  scroll-cache trick — reusing a level's own buffer across its own successive
  updates instead of a full re-render — and the texel-snap above buys that for
  free.

## Carve correctness needs a convex LOD field — ours isn't, by default

- The cascade-per-LOD carve excludes finer-LOD-active regions from a coarser
  cascade's rasterization. A cheaper-looking alternative — render everything
  into one depth buffer, tag carved fragments, discard tagged texels at merge
  time — is **not equivalent** and can silently drop real occluders.
- Why: a shadow map keeps only the nearest fragment per texel. If a carved
  (near) fragment happens to be nearer to the light than a legitimate (far,
  non-carved) occluder sharing the same light-space column, the legitimate
  occluder loses the depth test and is never even written — there's no "second
  place" to recover after the fact. Tag-then-filter can only inspect the
  winner; it can't resurrect what the winner buried.
- **This can't happen if the carved region is convex per light ray.** Two
  points sharing a shadow-map texel are collinear (same light-space column); if
  both a carved fragment and a receiver sit inside a convex carved volume, the
  whole segment between them — including anything "sandwiched" between them —
  is inside it too. So a non-carved occluder can never be the thing hidden
  behind a carved one. Box- and sphere-shaped LOD thresholds are both
  individually convex — that's not where the risk is.
- **The risk is chunk quantization.** An axis-aligned (Chebyshev) threshold,
  evaluated per-chunk on an axis-aligned chunk grid, quantizes to *exactly* a
  box — zero distortion, provably convex, and cheaper to compute
  (`max(abs(dx),abs(dy),abs(dz))`, no sqrt) than a sphere test. A spherical
  threshold quantized the same way produces a staircase boundary, which is
  **not convex** (classic L-shaped-notch problem) — reopening the
  dropped-occluder case in a thin ring right at the LOD boundary.
- **Our chunk residency is spherical** (`SphereChunkSubscriber`), not boxed —
  chosen for even streaming in every direction. So the naive
  single-buffer-plus-tag carve is not exactly correct for real LOD transitions.
- **Fix: one geometry pass, two depth outputs**, not two rasterization passes.
  Tag each draw with its existing carve flag. Fragment shader writes true depth
  to `own_full` unconditionally; writes true depth to `own_carved` only for
  non-carved fragments, and a sentinel (+∞) for carved ones. `own_carved`'s
  depth-test race excludes carved fragments entirely, so a legitimate occluder
  behind one always wins on its own merits — no dropped occluders, no second
  geometry submission, just one extra depth-sized buffer per real-LOD cascade.

## Close geometry casting onto farther cascades

- `own_full` (uncaved, includes near/carved-region geometry) feeds a cascade's
  **own native-tier receivers**. Needed because a tall or large caster near the
  camera can have a shadow throw (`height / tan(sun_elevation)`) that outruns
  its own cascade's footprint under a low sun — without this, that caster's
  shadow simply never reaches the far receivers it should.
- `own_carved` (sentinel-masked, previous section) feeds the **merge/OR chain**
  consumed by finer levels' far-occluder check. Excluding near geometry there
  isn't about correctness of the occluder fact itself — it's about not
  polluting a near receiver's already-precise native answer with a
  lower-resolution, higher-bias duplicate of the same geometry
  (peter-panning/self-shadow mismatch at the LOD seam).
- This distinction **only matters at real LOD transitions**, where mesh detail
  actually changes between neighboring cascades. It doesn't apply between
  cascades rendering identical mesh (see LOD0 below) — there's no detail
  mismatch to protect against.

## LOD0 isn't one cascade — it's a small ordinary CSM stack

- Every real LOD *n* ≥ 1 renders only the annulus `(R_n/2, R_n]` (main-view
  rendering already excludes the inner half — the finer LOD underneath handles
  it) — a fixed 2:1 distance band, always bounded away from the camera.
  **LOD0 alone renders the full disc `(0, R_0]`**, reaching all the way to
  contact distance, which is the one place shadow crispness (contact shadows,
  high-frequency penumbra) actually matters perceptually.
- For a fixed cascade resolution, texel-to-native-feature-size ratio works out
  to `2·radius_end / texRes` — **independent of the LOD level**, because
  footprint radius and native feature size both double every level and the
  `2ⁿ` cancels. So one cascade per real LOD (n≥1) is exactly enough forever,
  regardless of render distance or LOD count. LOD0 has no such fixed band to
  exploit — it has to cover everything from contact distance to `R_0` at once,
  which one buffer can't do without being either too coarse up close or
  absurdly large overall (e.g. ~20 texels/voxel at contact distance over a
  640-voxel-diameter footprint implies a ~12,800² texture).
- Chunk granularity kills carving as an option here directly: a shell radius
  near or below `CHUNK_SIZE` has no clean per-chunk in/out decision to make.
- **Resolution: plain graduated CSM, not the carve+OR machinery.** A handful of
  nested cascades sharing the *same* LOD0 mesh (e.g. for
  `radius_end=10, CHUNK_SIZE=32`: radii 20/40/80/160/320 voxels), each
  independently frustum/AABB-culled and rendered whole — no carving, no
  internal OR-merge, since nothing is excluded there's nothing to miss. A
  receiver just picks the tightest cascade that contains it, exactly like
  textbook CSM (except these are spherical shells around the camera, not
  view-frustum Z-slices, since the underlying LOD field is spherical too).
- **Bonus:** the outermost LOD0 cascade (`r=R_0`) is complete/uncaved by
  construction — it already *is* the `own_full` buffer LOD1's receivers need at
  that seam. LOD1 can OR-check it directly instead of needing a separate
  own_full pass there. (LOD1, LOD2, … still need their own dual-buffer
  treatment for casting *further* out, since they still carve internally
  against real LOD boundaries.)

## Shading-time cascade selection

- **LOD1+:** the cascade index is implicit in which draw call rendered the
  pixel (same active-LOD field that picked the mesh) — no runtime search.
  Sample `own[n]` + `merged[n+1]`: two fetches.
- **LOD0:** genuine runtime pick, since its stack shares one mesh tier. Compute
  3D distance from the fragment to the camera and compare against the (small,
  fixed) shell-radius array — implement **branchless** (step-function
  arithmetic building an index, not `if/else`), so lanes near a shell boundary
  diverge in *data*, never in *instruction stream*. Sample `own[shell]` +
  `merged[LOD1]`: also two fetches.
- **Total per-pixel cost**: one branchless select + ~2 shadow texture fetches
  (the coarser-merge one samplable at half/quarter-res, being low-frequency) +
  a small PCF kernel (~4 taps) for softening. Bounded, coherent, independent
  per pixel — the same cost class as any ordinary single/multi-cascade shadow
  lookup already shipping on mobile and integrated GPUs. Contrast with the
  original per-pixel DDA march this replaced: that died from *unbounded,
  incoherent, dependent* memory access; this is *bounded, coherent,
  data-parallel* — the difference the whole CSM decision was made to buy.

## Direct lighting: many clustered lights + few shadows

**Light count ≠ shadow count.** Conflating them is the classic mistake.

- **Clustered/tiled lighting makes light *count* nearly free** — cost scales with
  *local* light density (lights overlapping one pixel's cluster), not total
  count. Thousands of lights = fine.
- **Shadows are the cliff** — each shadow-caster = a geometry re-rasterization
  from the light's POV (point = 6 cubemap faces, spot = 1). 50 shadowed point
  lights = ~300 extra passes.

**So most lights must not cast real shadows.** Triage:

1. **Sun** → CSM. One hero shadow, everywhere.
2. **Small budget of hero lights** (searchlight, key lamps) → real shadow maps,
   importance-ranked, N small. **Prefer spotlights over omni points** (1 face vs
   6). Anything important enough to shadow can often be a clipped cone.
3. **The mass of small local lights** (torches, indicators, crystals) →
   **clustered, unshadowed direct.** Their occlusion is faked perceptually by:
   - **GI's visibility term** (occludes their *bounce* for free),
   - **AO** (grounds contacts),
   - **short attenuation radius** (hides the direct leak).
   The eye keys on big (sun) and bright-dynamic (searchlight) shadows; it doesn't
   miss the shadow under torch #47.

The tier-2-vs-3 decision is **brightness/importance, not light type.**

**Make the few real shadows nearly free: cache them.** A shadow map is identical
frame-to-frame for a static light over static geometry → render once, reuse.
Per-light dirty flag, re-render only when the light moves or a voxel **in its
range** changes. In a mostly-static voxel world this amortizes to ~free
(simplified Virtual Shadow Maps). Cap shadow work with a fixed atlas + importance
budget; lights stream in/out of shadow-casting as the player moves.

**Likely bugs in the prototype's clustered direct lighting** (to check before
rewriting): wrong light radius/attenuation (washed-out or every cluster swamped),
wrong cluster Z-slice bounds (flicker / missing lights), double-counting (over-
bright overlaps), lighting done in the wrong space / missing N·L or distance.

## Global Illumination: probe irradiance field (DDGI-style)

**The structural cure for "downscaling looks ugly": decouple lighting resolution
from screen resolution.** Trace rays from sparse probes, not from pixels.

How it works:
- Fill space with a 3D grid of **probes** — for us a **cascaded/sparse grid
  around the camera** (dense near, coarse far; must be *volumetric* for a fully-3D
  world with caves/overhangs, not a 2D plane).
- Each frame each probe shoots a handful of rays — **DDA marching the voxel
  grid**. Each ray gathers: surface **emission** + **direct lighting** +
  **irradiance already stored in nearby probes** (feedback = infinite bounce).
- Integrate into a tiny octahedral irradiance map per probe + a depth/visibility
  map (the chebyshev test that stops light leaking through walls).
- **Temporal accumulation** (~20–40 frames to converge).
- **Shade a pixel** = sample the 8 surrounding probes in the normal direction,
  weight by distance + visibility. A few texture reads. **Cost independent of
  light count and of GI screen resolution.**

Three properties that explain everything:
1. **Low-frequency** — soft broad diffuse fill; cannot do sharp edges, small
   bright spots, or crisp speculars (smeared to probe spacing).
2. **Laggy** — responds over tens of frames.
3. **Light-count-independent** — 1 light or 1000 cost the same.

**Direct/indirect split (the key mental model):** GI does **not** do direct
lighting. The sharp first hop (torch pool, spotlight cone) = clustered lights +
shadow maps (instant, sharp). **GI only adds the bounce.** Most "does GI break?"
worries are really "I asked GI to do a direct-lighting job."

**Emissive voxels light the world for free through the GI march.** Mark a
material emissive (color + intensity, zero art skill); probe rays hit it and
gather its emission. Lava / crystal / ore / fungi light entire caves with **no
per-light management**, scaling to millions of glowing voxels. Multi-bounce via
the probe feedback loop.

### Honest GI failure modes

- **Light leaking** through thin walls (probe sees a lit room through a 1-voxel
  wall). The depth/visibility term mitigates it, but **thin voxel walls vs coarse
  probe spacing are the classic voxel-GI leak** — plan wall thickness and probe
  density together. This is the bug you'll fight most.
- **Small/thin features** smeared to probe spacing → back them with a direct
  light or bloom.
- **Latency** is inherent → sharp/instant things go in the direct path.
- **No specular** — probes are diffuse only; glossy reflections (wet floor under
  lava) need a separate path (SSR or reflection probes). Later polish.
- **iGPU budget knobs**: probe density, rays-per-probe-per-frame,
  probes-updated-per-frame, octahedral resolution, cascade count.

## Making the sunless world interesting (caves / night / unlit)

The sun is a crutch that does art direction for free (form via gradient, depth
via shadow, mood via color). Pull it out and you lose all three → flatness. But
**sunless scenes are the strongest identity opportunity** — not competing with
every blue-sky game. Make the dark the best-looking part.

- **Centerpiece: emissive voxels + indirect bounce.** What separates a boring
  cave from an atmospheric one is the *bounce*, not the light. Free from the GI
  system — same system, fed by emitters instead of the sun. This is the highest-
  leverage answer and it's also gameplay (navigation, landmarks, danger cues) and
  identity.
- **Supporting cast (impact-per-effort order):**
  1. **Volumetric fog / light made visible** — depth, mood, hides LOD pop. Tier
     hard for iGPU (analytic height/distance + simple in-scatter on the floor;
     froxel volumetrics on desktop). The most likely thing to be too heavy.
  2. **HDR + exposure + bloom on emissives** — makes glowing voxels *glow*, keeps
     dark scenes legible instead of crushed-black mud. Cheap, runs everywhere.
     Don't skip — flat LDR is half of why dark looks boring.
     - *Why cheap:* bloom is low-frequency, so never blur wide at full res.
       Downsample into a mip pyramid (½ → ¼ → … → ~1/64), blur small at each
       level, then upsample-accumulate (Jimenez/COD-AW; dual-Kawase for iGPU,
       bandwidth-bound). A 5-tap blur at 1/32 res = ~160px glow at full res. Each
       mip is ¼ the pixels above it, so the whole pyramid ≈ 1.33× the half-res
       pass. Tiering = clamp pyramid depth (one knob, same shader). Bilinear
       half-texel taps integrate a 2×2 average for free.
     - *Drive from emissive intensity, not a luminance threshold* — materials
       already carry `(color, intensity)`; a hard luma cutoff is fragile and pops.
       Soft knee + Karis average (`1/(1+luma)` weighting) on the **first**
       downsample only kills the emissive/bloom fireflies flagged below.
  3. **PBR material response** — normal + roughness catching local lights. Kill
     the matte Lambertian look (guaranteed flat at night). Specular highlights
     read as detail in near-darkness.
  4. **Color-temperature contrast + non-black darkness** — warm torch vs cool
     crystal; a dark desaturated blue/purple "darkness" tint keeps silhouettes
     legible. Parameter-level art direction, not assets.
  5. **AO as form-giver** — modulates the fill/GI; useless alone (uniform gray),
     essential with bounce light.
- **Not a new renderer:** same probe GI + clustered lights + fog, wired to
  emissive inputs and tuned. Only volumetrics needs hard tiering. The sunless
  case is largely *free* once daytime GI exists.
- The remaining work is **design**: procgen placing emissive features for focal
  points, landmarks, and pools-of-safety-vs-threatening-dark composition.

## GI stress-test cases (how the system holds up)

- **Big cave, lava lakes, ores/crystals → GI's best case.** Lava = ideal
  low-frequency **area emitter** via emissive voxels (do NOT turn it into
  thousands of point lights). Torches = clustered direct (sharp pool) + GI bounce
  (warm fill). Small crystals are sub-probe-spacing → smeared → back a hero
  crystal with a cheap direct point light + bloom for the crisp glow.
- **Factory / city, many colored lights, cones → mostly a clustered-lighting
  question (already solved), not GI.** Direct = clustered (sharp cones, instant);
  GI adds their colored bounce at a cost *independent of light count*. Don't
  expect GI to draw the cone — it draws the cone's bounce.
- **Helicopter searchlight → GI is the wrong tool, but it was never GI's job.**
  Cone + lit circle = **direct spotlight + its own perspective shadow map**
  (instant, tracks the heli). Visible beam = **volumetrics**. Only the dim bounce
  lags, dominated by the direct cone → invisible. "A simple per-pixel direct on
  top for the few moving hero lights" is exactly standard practice.
- **Breaking a light emitter → the scary part vanishes instantly.** Direct light
  drops immediately (clustered just removes it). Only the dim indirect bounce
  lingers, decaying over the temporal window (tunable). **Event-driven probe
  invalidation** (accelerated local convergence near the change) makes a snappy
  ~10-frame fade achievable.

## Anti-aliasing and transparency

### Decision: TAA, not SMAA — and not for the first reason that comes to mind

**TAA over SMAA**, because SMAA only fixes geometric silhouette edges. Our real
aliasing budget is **specular/PBR sparkle + emissive/bloom fireflies** (and water,
below) — only a *temporal* method resolves those. And TAA is the **on-ramp to
FSR2-style temporal upscaling**, the actual lever for 1080p/60 on the iGPU floor
("decouple cost from screen res"); SMAA is a dead end with no upscaling path.

**Corrected rationale — do not repeat the wrong version.** This is *not* justified
by "DDGI gives us the temporal infra for free." DDGI accumulates in **probe/world
space** (EMA into octahedral irradiance maps) — no motion vectors, no screen
reprojection. That is temporal *accumulation*; TAA is temporal *reprojection*
(motion vectors + history + neighbourhood clamp). Different machinery, built
separately. Keep the two temporal tracks mentally distinct.

Build it as: Halton **jitter** → reproject via **motion vectors** → **variance clip
in YCoCg** (not per-channel min/max) → blend → **CAS/RCAS sharpen** to protect
crispness. Add a **reactive/responsive mask** (Teardown's "Motion + reactive mask")
for moving/transparent pixels — the standard fix for the clamp's color-similarity
blind spot (it cannot reject stale history that happens to match the local color box).

- **Clamp cost is cheap on iGPU:** the 9-tap 3×3 neighbourhood (or 5-tap cross /
  Karis "rounded" box) is *coherent local* reads — the good memory pattern, the
  opposite of the incoherent per-pixel marching that killed the old sun shadow.
- **Honest downsides:** ghosting/softening (mitigated by clamp + sharpen, and
  on-brand — we de-Minecraft via lighting, not crisp pixels); needs motion vectors
  (camera-only MVs reconstruct from depth for static geometry = cheap; only the few
  dynamic objects need real MVs). Our slow, mostly-static factory camera is TAA's
  best case — the ghosting failure modes are fast-motion/dynamic-content problems.
- **Sequence late** (around build step 5, with/after GI), never early.

### Transparency: composite *after* the resolve

Pipeline: opaque (jittered) → shade → **TAA resolve** (stable buffer) → transparent
passes after, rendered **unjittered** (or they wobble against the de-jittered image),
depth-**test** against opaque depth, depth-**write off** (so panes don't occlude each
other in the depth buffer and you can blend them).

- Consequence: transparent surfaces get **no TAA** → silhouette/specular shimmer
  under motion. Accept it (slow camera) or run a cheap edge AA on the transparent
  pass *only*. Do **not** pull transparents before the resolve — that reintroduces
  the velocity/ghosting/reactive-mask mess and fights the accumulation model below.
- **Alpha-test (cutout: foliage, grates, fungi) is not this problem** — opaque
  per-pixel, writes depth+motion, TAA handles it (pairs with hashed alpha). Only
  **alpha-blend** is hard.

### Stacked glass + water → no OIT needed (the material physics dissolves it)

The player can stack glass and water arbitrarily. Looks like it forces OIT; it
doesn't. Decompose each layer into commutative parts:

- **Absorption/tint → multiplicative → commutes** (order-independent).
- **Emission/glint → additive → commutes.**
- Non-commutative: **refraction + Fresnel reflection only.**

So **accumulate tint multiplicatively + emission additively across the whole stack
in any order — no sort, no OIT, correct for the bulk.** Handle reflection/refraction
for the **front-most reflective surface only**; multiply everything behind it by the
intervening tint (deeper reflections are attenuated → error invisible). Glass in
front of water falls out naturally.

This already **beats Minecraft**, which uses **no OIT** — it sorts translucent quads
back-to-front (painter's) and historically *couldn't blend stacked translucents at
all*. Sodium fixed it with **smarter sorting** (GFNI — exploits voxels' 6 face
normals — + quad splitting), still not OIT.

Back-pocket upgrades, only if a stacked case ever looks wrong, in order:
1. **Back-to-front sorting** — cheap for us via the 6-normal GFNI trick.
2. **Weighted Blended OIT** (McGuire & Bavoil 2013) — single-pass, approximate,
   bounded memory; the *only* OIT worth running on an iGPU.

**Avoid** per-pixel linked-list / A-buffer / depth-peeling — unbounded memory and
bandwidth, the iGPU-killer.

### Water specifically — skeleton holds, the nice properties don't

Same frame slot, a tier harder; belongs in the **later SSR/specular polish** stage,
not with glass.

- **Depth buffer becomes a read source:** `thickness = sceneDepth − waterDepth` →
  **depth-based absorption** (Beer-Lambert: shallow clear, deep dark — still a
  multiplicative tint) + **soft shorelines** (alpha fade as thickness → 0).
- **Refraction** (single-layer): sample the resolved opaque buffer with a
  normal-driven offset; **reject samples closer than the water surface** or front
  objects bleed onto the water in front of them.
- **Reflection (the real cost):** Fresnel-weighted, needs **SSR and/or a reflection
  probe** — i.e. exactly the deferred specular path (probes are diffuse-only; glossy
  needs SSR/probes, "later polish"). Water is where that path comes due.
- **Specular aliasing:** animated normals sparkle, and with no TAA on water it
  crawls → fix at the source with **prefiltered normals / specular AA** (Toksvig /
  LEAN), converting normal variance to roughness. The substitute for the TAA water
  doesn't get.

## Draw submission and GPU geometry memory

> Captured 2026-09-23 from a read-through of the old `voxel_engine`'s indirect /
> paged draw path (mechanics are in `VOXEL_ENGINE_ANALYSIS.md`, "Rendering
> pipeline — memory layout, culling, draw submission"). Hardware claims are from
> general knowledge, not checked against target specs; timing numbers are
> estimates. **Nothing here was measured** — see "Measure first".

### What indirect drawing does — and doesn't — buy

- **It does not make draws run in parallel.** wgpu submits to one queue and a
  render pass executes its draws in order. Consecutive draws with the same
  pipeline/state already overlap in the GPU's pipeline whether they are direct or
  indirect. Splitting LODs into separate passes would *not* run concurrently — it
  adds attachment load/store and barriers, so it is likely slower.
- **What it buys is CPU-side submission cost.** On the Vulkan backend
  `multi_draw_indirect` should be one call with a `drawCount` (my understanding of
  wgpu's Vulkan backend), so the CPU issues a handful of calls instead of
  thousands. On backends where wgpu loops it on the CPU (Metal, GL, DX12 without
  ExecuteIndirect, from memory) every sub-draw costs CPU time, and Metal has no
  `MULTI_DRAW_INDIRECT_COUNT`.
- **The old engine was not GPU-driven.** It frustum/backface-culled cached
  `(chunk, direction)` entries on the CPU, then `queue.write_buffer`'d the whole
  indirect buffer every frame (opaque + transparent), and issued one
  `multi_draw_indirect` per slab×LOD (`draw_voxel_geometry`, `render/mod.rs:1339`).
  The compute-culling item in the old `OPTIMIZATIONS.md` was never implemented.
- **Instancing is already how it works.** Each face is one instance (6 vertices, no
  index buffer) and chunk/direction/LOD come from
  `metadata[instance_index / PAGE_SIZE]`. So draw boundaries carry no meaning for
  correctness: one draw whose `instance_count` spans many chunks' pages renders
  correctly. **Many sub-draws exist only because culling leaves gaps** — a draw
  needs a *contiguous* instance range.

### Why many sub-draws cost something — and how much (with a correction)

Per sub-draw: the command processor fetches/decodes the 16-byte args and sets draw
state (`first_instance`); the GPU can't know neighbouring ranges are adjacent, so
it can't fuse them (merging is us doing that fusion in software); each draw rounds
up to whole waves; and a small draw (a full page is 9 waves of 64) only keeps the GPU
busy if the next draw overlaps it, so the limit becomes front-end draw rate rather
than shader throughput. Estimate ~0.1–1 µs per sub-draw (hardware-dependent):
~5k visible pages is likely under a millisecond, 100k+ can be several ms. Full pages
are wave-aligned (96 × 6 = 576 = 9 × 64), so only *partial* pages waste lanes.

**Correction:** the first-pass analysis framed tiny draws as the main problem. That
was overstated — the GPU already overlaps them and the per-draw cost is small.
Whether it matters is unmeasured.

**Measure first** (the old engine only has CPU-side timers —
`TIMING_FRUSTUM_CULL_US`, `TIMING_WRITE_INDIRECT_US`, `TIMING_DRAW_CACHE_ENTRIES`):
1. Sum `draw.count` across the draw list per frame → sub-draw count.
2. Timestamp queries around the shadow, main and transparent passes → is the GPU
   even the bottleneck, and in which stage.

If the frame is fragment-bound or the sub-draw count is in the low thousands,
leave the draw system alone.

### Options for fewer draws (smallest change first)

1. **Merge contiguous pages into runs.** Needs (a) *dense* runs and (b) address
   order — see "Locality-based batching" below.
2. **One draw per slab, cull in the vertex shader — rejected as the default.** It
   works (add a face count + dead flag to `PageMetadata`, using the spare 16 bits of
   `direction_and_lod`; frustum + backface test from the page metadata and camera
   uniform; the branch is wave-uniform because a page is 9 whole waves; culled
   instances emit one out-of-clip position for all 6 vertices so fixed-function
   rejects the triangles; **not** fragment `discard`, which rasterizes first). But
   **frame cost becomes proportional to all resident faces, not visible ones**:
   every culled face still launches vertex waves and goes through primitive
   assembly, and its 8-byte attribute fetch probably still happens (unverified).
   With a big view distance most faces are culled — backface culling alone drops
   about half — so this shades several times more vertices than it draws, and
   re-reads the whole slab every frame (worst on iGPUs). Only wins when most of the
   resident world is visible, or when submission-bound with idle vertex throughput.
3. **Hybrid: region-contiguous allocation + coarse CPU cull per region + per-page
   vertex-shader refine.** One draw per visible region (say 4×4×4 chunks), little
   wasted vertex work. Needs the allocator to place each region in a contiguous
   page range.
4. **GPU-driven compaction — one draw per pass.** A compute shader tests each
   chunk-direction entry (frustum, backface, LOD coverage), appends visible
   `page_index` values to a `visible_pages` buffer with an atomic, and writes
   `instance_count = visible × PAGE_SIZE` into a single indirect arg; drawn with
   `multi_draw_indirect_count` (needs the `MULTI_DRAW_INDIRECT_COUNT` feature). The
   vertex shader stops using vertex attributes and pulls the face itself:
   `page = visible_pages[instance_index / PAGE_SIZE]`,
   `face = faces[page * PAGE_SIZE + instance_index % PAGE_SIZE]`. Shadow, main and
   transparent passes can share the list or each get one from the same compute.
   Costs: partial last pages need a per-page face count and surplus instances get a
   degenerate position (exact packing needs a prefix sum); faces move from vertex
   attributes to a storage buffer — the old 128 MB slab is `174763 × 96 × 8 =
   134,217,984` bytes, **256 bytes over wgpu's default 128 MiB
   `maxStorageBufferBindingSize`** (limits may already be raised — unchecked; would
   need `PAGES_PER_SLAB` floored to 174,762); vertex pulling can be slower than
   fixed-function fetch on old dGPUs; and it is a real shader/buffer rewrite.
   Without indirect-count, culled entries must be zeroed (`instance_count = 0`)
   instead of removed, and each still costs a little front-end time.

The CPU cull + upload cost (option 4's motivation) is unknown until the two CPU
timers above are read. On iGPUs it matters less: unified memory makes
`write_buffer` ≈ a memcpy.

### Locality-based batching (space-filling / neighbourhood layout)

Idea: allocate so spatially adjacent chunks live in adjacent pages (2×2 / 3×3
neighbourhoods, or a space-filling curve), then batch consecutive ranges into one
draw. Verdict: right direction, and the batching step itself is ~free. Three catches:

1. **Runs must be dense.** A draw is one contiguous instance range, so a partial
   page in the middle of a run breaks it — and so does the `face_limit` truncation in
   `build_draw_for_direction` (standard vs total faces), which behaves like a partial
   page. Either accept a break at each chunk-direction's last page, or pad partial
   pages with zero-size faces (`w = h = 0`, rejected as degenerate triangles) at a
   cost of ~half a page of wasted instance slots per chunk-direction.
2. **Merging needs address order.** Sorting the draw cache per frame is wasted
   work. Instead keep a per-slab **visibility bitmap indexed by page** (174,763
   pages ≈ 2.7k `u64` words per slab): set bits for each visible chunk-direction's
   pages, then scan runs with `trailing_zeros` / `trailing_ones`. Already sorted, tiny.
3. **Layout key: direction-major, then Hilbert/Morton of chunk position.** Backface
   culling by direction is the biggest single cull (~half the faces). With a
   chunk's 6 directions adjacent in memory, visible runs alternate with culled
   ones; with direction-major layout a culled direction drops out as whole ranges,
   and a frustum cut through the curve yields few intervals. Hilbert gives the
   fewest intervals, Morton is simpler, a snake pattern is fine.

**LOD ordering interacts with this.** The old engine deliberately draws LOD 0 first
(`write_draws_to_indirect`; `VOXEL_ENGINE_ANALYSIS.md` "Draw ordering") so coarser
geometry behind it is depth-rejected instead of overdrawn. Address-merged runs that
mix LODs would break that ordering, so either put LOD into the layout key (an arena
per direction × LOD — my inference, not designed) or accept losing the ordering.
**Correction:** the first pass also suggested reordering draws slab-outer to cut
vertex-buffer/bind-group rebinds ("nothing needs LOD grouped"). That ignored the
early-Z purpose above, and with only 1–2 slabs the rebinds are negligible — drop it.

### Allocating variable-size chunk-direction meshes

Problem: each chunk has an undefined number of pages per direction, so what to hand
out, and how contiguous, is an allocator question.

| Strategy | Notes |
|---|---|
| Fixed pages + free list (old engine) | O(1), no external fragmentation, but no contiguity — every page can be its own draw |
| Variable-size blocks, **TLSF** | O(1) alloc/free, low fragmentation; the usual choice for GPU mesh arenas. Rust `offset-allocator` crate is a TLSF; I believe Bevy's mesh allocator uses it |
| Buddy | Power-of-two blocks, up to ~50% internal waste, very simple coalescing |
| Per-region arenas + compaction | Sodium-style (from memory): a buffer per region, arena with free-list segments, grow/compact by copying; per-chunk facing ranges stored contiguously so consecutive visible facings merge into one draw |
| Ring buffers | Per-frame/streaming data, not persistent meshes |

**TLSF (two-level segregated fit)** — variable-size blocks from one big range, O(1)
alloc and free:
- Free blocks live in bins by size. Level 1 = power-of-two class (`leading_zeros`);
  level 2 = each class split into e.g. 16 equal sub-bins. Two small bitmaps record
  which bins are non-empty.
- *Alloc(n):* round `n` up to the next bin boundary (so every block in that bin
  fits — "good fit", not best fit); find the smallest non-empty bin at or above it
  with `trailing_zeros` on the bitmaps; pop a block; if larger than `n`, split and
  return the remainder to its bin. Waste per allocation bounded ≈ 1/16 with 16
  sub-bins.
- *Free:* look at the block's physical neighbours, merge with any that are free,
  reinsert into the right bin.
- For GPU memory the block headers live on the CPU; the allocator only returns
  offsets (here: page numbers).

**What the old engine actually does** (verified in `PageAllocator::allocate` and
`upload_direction_faces`): pages are allocated **one at a time from the first slab
with a free page**; slabs are *direction-agnostic* — direction lives only in
`PageMetadata.direction_and_lod`. The "6 direction buffers per chunk" from the old
`RENDERING_ARCHITECTURE.md` was replaced by pages, so direction culling acts on draw
args (`CachedDraw.backface_culled`), not on memory layout, and the visible pages
are scattered. Fresh loads happen to come out chunk-major and ascending (the free
list is initialised in reverse and popped), but reuse is LIFO, so pages freed behind
the camera get reused ahead of it and locality scrambles over time.

**Recommended direction for the new engine:**
1. **Keep the page as the granularity** (`metadata[instance_index / PAGE_SIZE]` still
   needs it) and run a TLSF over page indices. Allocate a chunk-direction as **one
   contiguous run of N pages**, writing metadata for every page in it. No shader
   change.
2. **Fall back to several runs when no contiguous block exists.** `CachedDraw.args`
   is already a list, so this needs no new mechanism. Example: 250 faces = 3 pages;
   today pages 812, 40, 9001 → 3 draws; a run 812–814 → one draw with
   `first_instance = 812 × 96`, `instance_count = 250` (the partial last page is
   fine because it ends the run); no run of 3 free → 2 + 1 → 2 draws.
3. **Direction-major arenas** (one allocator per direction, possibly × LOD, see
   above) so backface-culled directions skip whole arenas. The alternative,
   chunk-major (one block per chunk holding all 6 directions), makes remeshing a
   single alloc but cannot make every camera octant's visible set contiguous: a
   chunk shows at most 3 of its 6 facings when the camera is outside its extent on
   every axis (both of an axis pair when the camera's coordinate lies within the
   chunk's range on that axis). Since direction culling is the biggest cull, prefer
   direction-major.
4. **Compact only if needed.** If large runs stop being available, defragment in the
   background with `copy_buffer_to_buffer`, a few blocks per frame.

**Frees (correction).** The first pass said deferred frees (by frames in flight)
were needed. With `queue.write_buffer` and buffer copies on one wgpu queue,
submission order plus wgpu's inserted barriers already protect against overwriting
a page a previous submission is still reading, so immediate frees are safe in the
current design. Deferral matters only for writes the queue doesn't order (mapped
staging buffers, async compute).

**Latent bug in the old engine — do not port.** `build_draw_for_direction`
overwrites `slab_index` with each page's slab and keeps only the last one for the
whole `CachedDraw`; `frustum_cull_cache` then groups *all* of its args under that
slab. Since allocation takes the first slab with a free page, a direction's pages can
straddle two slabs once slab 0 nearly fills, and the slab-0 pages would be drawn
with slab 1's vertex buffer. Found by code reading, not reproduced; only reachable
past ~16.8M resident faces. Allocating each run inside one slab removes it as a side
effect.

### Cheaper things before the fragment stage (independent of draw structure)

1. **6 vertices per face, no index buffer** → 6 vertex invocations per quad, each
   redoing the AO unpack, flip test and direction `switch`. A 4-vertex triangle strip
   cuts that by a third; the AO diagonal flip becomes a rotation of the strip's
   corner order.
2. **`out.normal` is a redundant `vec3` varying** — it's a pure function of
   `direction`, which is already output flat. Derive it in the fragment shader: 3
   fewer interpolants.
3. **Per-frame CPU frustum cull + full indirect upload** for opaque and transparent.
   Read the two CPU timers before deciding on compute culling (option 4 above).
4. **Geometry is submitted more than once.** The old shadow depth+normal pass
   (`shadow/pass.rs`) redraws all opaque geometry at reduced resolution, unjittered,
   so it cannot double as a depth prepass for the jittered full-resolution main
   pass. A full-res jittered depth prepass would let `fs_main` run once per pixel via
   early-Z, at the cost of another vertex pass. *Inference for the new engine:* CSM
   cascades (see above) each re-submit geometry too, which raises the value of
   cheap submission (shared visible list from one compute pass, or a coarse region
   structure reused by all passes).

### Hardware tiers

- **Modern iGPUs** (Intel Xe/Arc, AMD RDNA2/3 APUs, Apple M-series): full compute,
  storage buffers, multi-draw-indirect. The difference is memory, not features:
  shared DDR (~50–100 GB/s vs 300+ on a dGPU), so bandwidth is the scarce
  resource. Keep faces compact (8 B), minimise full-screen targets (TAA history,
  WBOIT accumulation, the shadow pass), offer a render scale, cut varyings. CPU
  culling is cheap (no PCIe; `write_buffer` ≈ memcpy), so GPU-driven culling gains
  less — but CPU and GPU share a power budget. **Apple GPUs are tile-based:** use
  `DontCare`/`Discard` for depth you don't need, skip depth prepasses (hidden
  surface removal already handles overdraw), and note wgpu-Metal loops
  `multi_draw_indirect` and has no indirect-count.
- **Old dedicated GPUs:** DX11-class or newer (roughly Kepler / GCN 1.0, 2012+) have
  compute and indirect draws but low compute throughput, and indirect-count may need
  newer drivers or an extension. Older than that: no compute, often no Vulkan, wgpu
  would need the GL backend — not worth targeting. **VRAM (often 1–2 GB) is the
  main limit**, so slab size / view distance / LOD budget matter more than draw
  count; lighting fragment ALU also weighs heavier there.
- **Design consequence:** keep **CPU cull + `multi_draw_indirect` as the
  baseline** (works on any Vulkan device with the multi-draw feature; wgpu loops it
  elsewhere) and make GPU-driven compaction an *optional* path gated on
  `adapter.features()` (`MULTI_DRAW_INDIRECT_COUNT` + compute). The old engine
  hard-requires `POLYGON_MODE_LINE | INDIRECT_FIRST_INSTANCE` (`main.rs:175`), so
  adapters lacking either fail at startup: `POLYGON_MODE_LINE` is only for
  wireframe → make it optional; `INDIRECT_FIRST_INSTANCE` is load-bearing for the
  `first_instance = page_index × PAGE_SIZE` scheme, so it is effectively the
  feature floor.

## Recommended build order

1. Greedy-meshed LOD chunks, rasterized. **Baked vertex AO** at mesh time (free,
   ~60% of "good voxel lighting").
2. Sun via **CSM** from LOD meshes (cascade-per-LOD + carved geometry).
3. **Clustered direct lighting** (fix/replace the prototype's), unshadowed for
   the mass of lights.
4. Hero-light shadow maps with caching + importance budget.
5. **Probe irradiance GI** (emissive + sky + sun + local bounce). Reuse the DDA
   marcher. This is where it stops looking like a game.
6. Volumetric fog + HDR/exposure + bloom (tiered for iGPU).
7. Coarse sun-occlusion march for caves/distant occluders (if not folded into
   cascades) — shares the GI sun-visibility query.
8. **TAA** temporal resolve over the opaque pass (after GI, once specular/emissive
   aliasing is visible enough to matter): motion-vector + depth prepass → jitter →
   variance-clip (YCoCg) → reactive mask → CAS/RCAS sharpen. On-ramp to FSR2-style
   temporal upscaling for the iGPU floor.
9. **Transparency** — composite after the resolve (unjittered, depth-test/no-write).
   Glass via commutative tint+emission accumulation (no OIT, any order). Water folds
   into the later SSR/reflection-probe specular polish.

## Open questions / pending decisions

- **Hardware floor**: "5-year-old laptop iGPU @ 1080p/60" vs "modern iGPU / Steam
  Deck"? Sets probe/shadow/volumetric budgets.
- **How low can the sun go?** Permanent grazing sun stresses far-occluder
  shadows hard; sun staying above ~20° makes it much cheaper.
- **Destructible/editable voxels — how frequent?** Drives shadow-cache
  invalidation and probe-invalidation design.
- **Are caves/enclosed interiors a core pillar or occasional?** If core → coarse
  sun-occlusion march is always-on and first-class.
- **Keep colored transparent shadows?** If yes, a sparse voxel-march pass shares
  the grid with GI; if no, the whole grid/bitmask subsystem can be deleted in
  favor of CSM.
- ~~Is the LOD field strictly concentric or can it be irregular?~~ **Resolved
  for carving purposes**: chunk residency is a quantized sphere
  (`SphereChunkSubscriber`), which is not convex, so real-LOD carving needs the
  own_full/own_carved dual-buffer fix rather than a cheap single-buffer tag
  (see "Carve correctness needs a convex LOD field" above). Still open if
  visibility/importance-driven irregularity is wanted for reasons other than
  shadows — that would need revisiting this fix.
- **Any frosted / partial-coverage transparent material?** That's the one case the
  commutative tint+emission accumulation can't do (true order-dependent coverage) →
  would force sorting or WBOIT. Clear glass + water don't need it; frosted would.
- **Water reflection: SSR, reflection probes, or both?** Ties directly to the
  deferred specular path and sets the water polish budget.
- **How much fast camera / dynamic motion?** Sets how hard TAA's reactive mask + MV
  quality must work — a slow builder camera is the easy regime, vehicle/flight cams
  are not.
- **Is draw submission actually a bottleneck?** Unmeasured. Needs the per-frame
  sub-draw count and GPU timestamp queries per pass; gates everything in "Draw
  submission and GPU geometry memory". If the hardware floor decision above lands on
  the older/weaker end, CPU-side culling cost vs GPU-driven compaction changes too.
- **Geometry memory layout:** direction-major arenas (× LOD?) with TLSF over page
  runs vs chunk-major blocks; region granularity for the hybrid cull; whether to pad
  partial pages with degenerate faces to keep runs dense.
- **Is `INDIRECT_FIRST_INSTANCE` an acceptable feature floor?** The page-metadata
  scheme depends on it; dropping it needs another way to get the page index to the
  shader.
