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
- **Is the LOD field strictly concentric** or can it be irregular (visibility/
  importance-driven)? Decides whether carving is a trivial radius check or a real
  "all-finer-loaded" query.
- **Any frosted / partial-coverage transparent material?** That's the one case the
  commutative tint+emission accumulation can't do (true order-dependent coverage) →
  would force sorting or WBOIT. Clear glass + water don't need it; frosted would.
- **Water reflection: SSR, reflection probes, or both?** Ties directly to the
  deferred specular path and sets the water polish budget.
- **How much fast camera / dynamic motion?** Sets how hard TAA's reactive mask + MV
  quality must work — a slow builder camera is the easy regime, vehicle/flight cams
  are not.
