# Engine & UI Design Notes

> Design discussion captured 2026-06-14. Context: Rust voxel engine, **hard
> low-end iGPU requirement**, a **custom voxel-march renderer as the core
> thesis** (see RENDERING_NOTES.md), already using `bevy_ecs`. Values control,
> fast iteration, and not reinventing the non-rendering 80%.

## The core tension

The project's defining choice (a custom voxel-march renderer for low-end
hardware) is in **direct tension** with what big engines optimize for. The thing
that makes the project distinctive is exactly the thing a full engine makes hard.
Every decision below flows from that.

## Engine ladder (summary)

- **UE5** → only sensible if abandoning the custom-renderer thesis and targeting
  mid-range. Contradicts the project goals.
- **Bevy** → keep the custom renderer + low-end + Rust; get scaffolding; lose
  maturity/editor; eat render-API churn.
- **Fully custom** → max control, max non-rendering workload.
- **Sweet spot found:** `bevy_ecs` standalone + `winit` + `wgpu` + `egui`
  (and/or an `epaint`-backed custom UI). Keeps the ECS we like, owns the renderer
  outright, escapes Bevy's render-world churn. **This is the recommended target.**

## Unreal Engine 5 assessment

- **Editor lag ≠ shipped-game perf.** Cooked builds + scalability are far
  lighter; Fortnite ships on phones on UE5. **But** UE5's headline features
  (Lumen GI, Nanite geometry) carry high baseline cost, target mid-to-high
  hardware, and Nanite often won't run on older iGPUs. For low-end you'd **turn
  both off** — removing the main reasons to be on UE5's renderer.
- **Customizability is tiered.** Vast at gameplay / materials / Blueprints.
  **Brutally hard at the renderer tier** — custom GPU passes mean working in the
  Render Dependency Graph (RDG) on the render thread in C++, global shaders, and
  likely **engine-source modification** + custom builds (100+ GB, long compiles,
  slow iteration). That tier is exactly where voxel rendering lives.
- **Custom voxel rendering, specifically:**
  - Easy path = generate meshes (greedy/marching cubes) fed as **normal
    geometry** (Voxel Plugin etc.). This is exactly what we *don't* want, and it
    routes through Nanite/Lumen/VSM (heavy, low-end-hostile).
  - Our-way (custom march) = the RDG/source-surgery path **and** Lumen can't use
    our voxel grid (it has its own SDF/surface-cache scene representation) → we'd
    reimplement GI from scratch inside an engine whose GI we're not using. Worst
    of both worlds.
- **Genuine benefits** (mostly orthogonal to rendering): mature editor + asset
  pipeline, physics/animation/audio/networking/UI, packaging to many platforms,
  and the **marketplace / Fab / Quixel** + material editor (helps the
  "not-an-artist" worry — but assets are overwhelmingly realistic *mesh* content,
  may not fit a voxel aesthetic).
- **Verdict:** wrong fit. For a renderer-first, low-end, custom-voxel project you
  would fight UE5 at its hardest tier while disabling its best features. The
  steelman for *an* engine (don't reinvent the 80%) is real, but UE5 is the wrong
  engine to offload to because it fights the renderer.

## Bevy renderer pain — validated, with the cause

The "convoluted/messy" feeling has a specific cause: **two ECS worlds.** The game
lives in the **Main World**; rendering runs in a separate **Render World**
(sub-app, mostly wiped/rebuilt per frame). Data crosses via a fixed pipeline:

**Extract** (Main→Render) → **Prepare** (GPU buffers/bind groups) →
**Queue** (build draw items into render phases) → **Render** (render graph nodes
record command buffers).

That split + the phase/draw-command machinery for Mesh+Material is the
convolution. It is also the **upgrade-fragile surface** — it churns most releases.

## Fitting a custom voxel renderer into Bevy (Config A)

Keep Bevy's render stack (so `bevy_ui` works) and inject the renderer as a node.
The reframe that makes it tractable: **bypass Mesh/Material/RenderPhase entirely;
the less of Bevy's mesh system you touch, the less pain.**

1. **Trim:** `DefaultPlugins.build().disable::<PbrPlugin>()` (+ gltf/sprite if
   unused) sheds the 3D mesh/material/lighting path we're replacing. Can't remove
   the core `RenderPlugin` / core-pipeline / `UiPlugin` — UI rides on them.
2. **Share the wgpu device:** pull `RenderDevice`/`RenderQueue`/`RenderAdapter`
   (thin wgpu wrappers), or hand Bevy a pre-created device via
   `RenderPlugin { render_creation: RenderCreation::Manual(...) }` (verify exact
   name per version). Manual is attractive — pick adapter/features/limits for
   low-end, Bevy shares the same device.
3. **Own your voxel GPU resources** in a render-world `Resource` (your buffers,
   bitmask grid, pipelines, bind groups). Skip most extract/prepare/queue — just
   upload chunk diffs.
4. **One custom `ViewNode`** in the `Core3d` subgraph **before** the UI pass; use
   `render_context.command_encoder()` (raw `wgpu::CommandEncoder`) to record your
   prototype's passes verbatim into the camera `ViewTarget`. UI composites on top.
5. **Stop Bevy double-processing:** `Tonemapping::None`, manage clear color,
   match the ViewTarget HDR format.
- **Cost:** you keep (and must slot into) the render-world/graph/camera-driven
  complexity — because **`bevy_ui` needs it**. You don't own the frame or present
  (you rent a node). The node + graph-label glue is the part that breaks on
  upgrades — keep it thin and isolated.

## The key insight: `bevy_ui` is not a non-graphics system

"Give me Bevy's UI + non-graphics, I'll do rendering" **cannot cleanly
separate** — `bevy_ui` renders *through* the render world / render graph /
camera-ViewTarget machinery. To get `bevy_ui` you must keep the whole render
complexity you disliked. So the question splits:

- **Config A (keeps `bevy_ui`)** = the node approach above. Inherits the
  render-world/camera/graph complexity + churn.
- **Config B (drops `bevy_ui`)** = `bevy_ecs` standalone + `winit` + `wgpu` +
  `egui`. Keep the ECS we like, own the renderer outright, `egui` for UI, build
  the rest (small focused crates). **Escapes the render-world churn entirely** —
  and you own compositing yourself, which is where Bevy is slowly migrating
  anyway.

`bevy_ui` is the one thing that drags you back into the render complexity. Drop
it and the rest of Bevy is genuinely à la carte. **Config B is the faithful
answer** for a renderer-first project; use `egui` for dev tooling immediately.

## "Cameras drive the render graph" migration status (mid-2026)

The instinct that this design is strange is **officially considered a flaw.**

- The model where each camera is a black box owning an internal `ViewTarget` and
  doing its own compositing causes ViewTarget/HDR confusion and multi-camera
  double-tonemapping/clearing bugs.
- **Discussion #19698** (June 2025) proposes the end-state: **cameras as "logical
  render passes" with explicit texture inputs/outputs**, compositing as its own
  explicit step → moving toward **render-target-driven** rather than
  camera-driven. That destination is *friendlier* to a custom renderer + UI
  compositing (it's the explicit compositing-camera model made first-class).
- **Shipping incrementally, no big-bang target.** Landed so far: 0.17 moved
  `Camera`/`Camera3d`/`Camera2d`/`ClearColor` into a new **`bevy_camera`** crate;
  `RenderTarget::None` / cameras without color targets (PR #20830, prepass-only).
  The big reframe is still in design.
- **Implication:** this layer is actively in flux = upgrade-fragile. More reason
  to keep the renderer behind a thin node (Config A) or go `bevy_ecs`-standalone
  (Config B), where none of this touches you.

## egui customizability

Very flexible on two axes, weak on the third:

- **Paint / widget — extremely flexible:** hierarchical `Style`/`Visuals`
  (colors, rounding, strokes, shadows, spacing, per-state widget visuals), custom
  fonts, custom widgets via the `Widget` trait or raw `Painter` (draw anything:
  shapes, text galleys, textured meshes, 9-slice), and **`egui-wgpu` paint
  callbacks** that inject raw wgpu draw commands inside the UI layer (e.g. a live
  3D viewport widget — great for an editor).
- **Layout — the weak spot:** native egui is immediate-mode with simple
  horizontal/vertical/grid layouts, **no flexbox**. Filled by **`egui_taffy`**
  (Flexbox/Grid/Block via **Taffy** — the same engine Bevy's UI uses) or the
  lighter `egui_flex`.
- **Aesthetic ceiling:** recognizable tool/Dear-ImGui look. Theming changes
  colors/spacing/rounding, but the *bones* read as egui unless you custom-paint
  most widgets. Superb for **editors, inspectors, debug HUDs, prototyping**; not
  art-driven AAA menus without heavy custom painting.

## "Professional game UI" reframe

The system "Taffy flexbox on top of egui" **already exists** (`egui_taffy`), so
this is *adopt + extend*, not build-from-scratch. But:

- **Flexbox was the easy 30% (now a dependency).** "Professional look" is the
  **70%**: a skin layer (9-slice panels, typography, icons, art), controller +
  keyboard **navigation/focus** (egui's tab-focus is not built for gamepad UI),
  **animation/transitions**, and a retained model. Mostly art/design + DIY, and
  egui's immediate-mode grain fights some of it.
- The layout engine makes UI **responsive and maintainable**, not professional-
  looking. That comes from design/art effort — same lesson as the world art.
- **Aim for "clean stylized," not AAA-skinned.** A clean, consistent, well-
  typeset stylized UI reads as professional and is achievable solo / non-artist;
  chasing art-driven AAA UI is a real art project.

## Custom UI framework on egui as a backend (recommended UI path)

You can use egui *underneath* a custom framework — and the clean version is
**easier than raw wgpu**. egui is three layers:

- **`epaint`** — font atlas + text layout + anti-aliased tessellation (shapes →
  wgpu-ready triangle meshes). The actual draw layer.
- **`egui`** — the immediate-mode context (layout, input, widget state).
- **`egui-wgpu` / `egui-winit`** — wgpu renderer + winit input feed.

For a **retained custom framework that does what you want**, stand on **`epaint`
+ `egui-wgpu`'s renderer** and **skip egui's immediate-mode layout/interaction.**
Your framework owns: retained tree, **Taffy** layout (with measure functions that
ask `epaint` for text sizes), input routing + hit-testing, focus/navigation,
animation, skinning. egui (epaint) is purely the paint/text backend.

**Why easier than direct wgpu:** the two hardest, most tedious parts of a wgpu UI
renderer — **text** (font atlas, shaping, layout into quads) and **anti-aliased
tessellation** — come solved, plus a working mesh renderer. You skip ~30–40% and
keep near-total control above it. **Bonus for low-end:** `epaint` renders
lightweight triangle meshes with a simple shader — much friendlier to iGPUs than
compute-heavy GPU-vector stacks (e.g. Vello).

**One decision that keeps it from getting messy — pick your layer, don't
straddle** the immediate-mode (egui) vs retained (your framework) boundary:

- Retained framework, own input/focus/animation → **`epaint` as a pure paint
  backend.** (Recommended.)
- Just want flexbox + egui's widgets with theming → **full egui + `egui_taffy`**,
  but then it's "egui themed," not "your framework."

**Honest scope:** `epaint` removes the rendering/text burden, **not** the
framework burden (tree, Taffy glue, input, focus, animation, skin remain yours).
"Easier than raw wgpu: yes. Easy: no." If `epaint`'s text proves insufficient
(complex scripts, fancy SDF effects), the drop-in upgrade is **`cosmic-text` +
`glyphon`** for text while keeping your own tessellation.

**The control/effort spectrum — choose consciously:**
1. Raw wgpu, everything → most control, most work.
2. **`epaint` + `egui-wgpu` backend + your retained framework (+ Taffy)** →
   strong control, text/tessellation free, low-end-friendly. ← recommended.
3. Full egui + `egui_taffy` + theming → least work, egui's grain + aesthetic
   ceiling (great for tools/dev UI regardless).

## Recommended path

- **Dev / editor / debug UI:** raw `egui` (+ `egui_taffy` for nice layouts) now —
  egui's sweet spot, near-zero effort, gives a real tooling layer.
- **Engine shell:** `bevy_ecs` standalone + `winit` + `wgpu` (Config B); pull
  other Bevy non-render crates (`bevy_asset`, `bevy_tasks`, `bevy_reflect`,
  `bevy_audio`) as libraries where useful.
- **Game UI:** start with themed `egui` + `egui_taffy` aiming for "clean
  stylized"; graduate to an `epaint`-backed retained framework only if/when UI
  becomes a core pillar needing more bespoke look/behavior.

## Open questions / pending decisions

- **Do you need `bevy_ui` specifically**, or just *good* UI? (Decides Config A vs
  Config B — and B is the one that matches "I'll handle rendering.")
- **Is UI a core pillar** (lots of menus/inventory/RPG screens) or HUD + a few
  menus? Decides custom-framework investment vs themed egui.
- **Tolerance for Bevy render-API churn** if going Config A.
- **Text needs** — Latin/common scripts (epaint is fine) vs full international
  shaping (`cosmic-text`/`glyphon`).

## References

- Bevy camera-driven rendering rework: discussions
  [#19698](https://github.com/bevyengine/bevy/discussions/19698),
  [#19704](https://github.com/bevyengine/bevy/discussions/19704);
  [PR #20830](https://github.com/bevyengine/bevy/pull/20830) (cameras without
  color targets); [0.16→0.17 migration](https://bevy.org/learn/migration-guides/0-16-to-0-17/).
- Bevy custom render pass: [custom_post_processing example](https://bevy.org/examples/shaders/custom-post-processing/),
  [render stages cheatbook](https://bevy-cheatbook.github.io/gpu/stages.html).
- [`egui_taffy`](https://github.com/PPakalns/egui_taffy) (Flexbox/Grid/Block via
  Taffy) · [`egui_flex`](https://crates.io/crates/egui_flex) ·
  [egui styling/theming](https://deepwiki.com/emilk/egui/5.1-styling-and-theming) ·
  [`hello_egui`](https://github.com/lucasmerlin/hello_egui) add-on crates.
