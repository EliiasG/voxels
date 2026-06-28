# Game Design Notes (abstract)

> Status: early design synthesis. Mechanics-and-philosophy level — no numbers, recipes,
> or final tech-tree layout yet. The *why* is recorded alongside each *what*, because the
> rationale is what should guide future decisions when specifics get filled in.

---

## 0. Premise (placeholder)

You operate an industrial **factory** on a safe, open **overworld**, and you drill a single
central **shaft** down into a stack of hostile **dimensions**. You pilot an upgradeable
**mech** to conquer each dimension and tap its exotic materials, feeding them back into the
factory to descend further. Fiction/setting is deliberately left abstract for now.

---

## 1. North star

This game is a deliberate **fusion of two automation traditions** the designer loves and wants
to join:

- **The modded-Minecraft / Stationeers tradition** — scale by *tiering up* (better machines,
  cleverer or more compact setups), with rich hands-on processes; but resources tend to get
  "solved" and then forgotten. Specific touchstones and what each contributes:
  - *Mekanism* — efficiency tiers (ore-doubling) and **byproduct-driven** chains (§2).
  - *Create* — **package logistics riding on physical transport** (the spine of §5).
  - *Refined Storage / AE2* — on-demand, addressed, centralized storage (the convenience pole of §5).
  - *Stationeers* — **hands-on, multi-variable manual processes**, especially alloying (§3).
  - *All the Mods*–style packs — the sprawling, many-system patchwork-base feel.
- **The Factorio / Satisfactory tradition** — scale by *replication* (more of the same machines,
  **blueprints**), with **physical belt/train logistics** and demand that keeps climbing so
  nothing is ever fully solved.

The whole design is an attempt to **bridge their core contradiction** — *tier-up-and-forget* vs
*replicate-forever* — via the §2 automation ladder and the principle below.

**The unifying principle: _scarcity migrates._** What is scarce keeps changing over the course
of the game, and every upgrade relieves the current bottleneck by leaning on the next one:

> labor → space/logistics → throughput → premium inputs / **attention**

No resource is ever allowed to become *free*. It becomes a *different kind of expensive*. This
is the single rule that most decisions should be checked against.

Two supporting commitments:

- The **factory is primary and always safe.** It is never under threat.
- **Two frictions, one factory:** the *overworld's* friction is **logistics**; the *dungeon's*
  friction is **combat**. Both pillars exist only to feed the one factory.

---

## 2. Progression model: manual → inconvenient → convenient

Every resource/process climbs an **automation ladder**, but **asymmetrically** — each at its own
pace. At any moment the base is a *patchwork* of tiers (some resources hand-gathered, some on
sprawling infra, some compressed into one humming machine). That patchwork is the texture; the
base never reaches a static "done."

- **Manual is the learning phase.** The manual process and the automated process are the *same
  underlying simulation*; only the control loop moves — from your hands to a machine. Automating
  something = *encoding your mastery into a controller*. This makes the manual phase genuine
  practice, not throwaway grind.
- **The manual thread migrates.** "Manual" is always whatever sits at the frontier and isn't
  automated yet. Early it's gathering; late it's piloting the mech into a new dimension. As a
  *share of activity*, manual work shrinks over the game (most systems are mature late; you
  introduce fewer new ones). When a manual tier *reappears* later it must be a **new or changed**
  process (new tool, new hazard, new wrinkle) — never literally "do the old thing again."
- **Two kinds of upgrade:**
  - *Tradeoff upgrades* (peripheral resources): compact/convenient but costs a premium input, so
    old and new tiers **coexist** as a real choice.
  - *Real/vertical upgrades* (core resources): genuinely better, **obsolete** the old tier, and
    are **forced by demand walls** (end-game demand simply outpaces what primitive tiers can
    sustain — even in infinite space, because feeding the spam drowns you in logistics).
- **Efficiency over throughput, with byproducts, is the anti-spam weapon.** Prefer upgrades that
  do *more per input* (ore-doubling style) over *more per time*. The killer detail is
  **byproducts**: higher tiers demand new reagents and emit byproducts you must use or void,
  which interlinks previously independent production blocks into a *web*. A web cannot be
  blueprint-spammed. The factory grows **deeper, not wider** — wider is the boring failure mode.
- **Interlocked tech tracks** pace the patchwork automatically: upgrading A requires something
  still primitive in track B, so you physically can't max everything at once.

---

## 3. Resources & media

Multiple media, each with **different transport physics** (so it's not five copies of one
logistics game):

- **Items** — discrete, packageable, buffer in containers. *(core)*
- **Fluids** — continuous, pipes, expensive to package → naturally live in the bulk-infra regime. *(core)*
- **Energy** — networks with **transmission loss over distance** (a *soft* spatial constraint:
  doesn't cap how big you build, just rewards local generation). Managed, not punishing. *(core)*
- **Gas** — like fluids but **compressible**: volume depends on pressure (compress to ship
  cheaply). Pressure as mechanic/hazard. *(mid-game)*
- **Mana / magic** — the deliberate **rule-breaker**: licensed to violate exactly one physical
  law everything else obeys (e.g. the one medium that transmits wirelessly, or one that can't be
  stored and must be made fresh). *(optional / speculative late)*

Each medium runs its **own** manual→infra→convenient arc, staggered in time.

**Cross-medium combination** is where the hard/interesting processes live (alloys etc.). Tune it
like Stationeers but **dialed back**:

- *Richness:* moderate (≈2–3 managed variables per process — a craft, never a part-time job).
- *Punishment:* low (a botched batch yields recyclable slag, not a destroyed base; failure costs
  materials/time, never progress).
- *Precision:* opt-in (sloppy ratios fine early; tight tolerances only for high-grade outputs).
- Variables are **legible** (show the gauges) — difficulty is *hitting* a visible target, never
  *discovering it exists*.

**Volume system** (Space Engineers / Stationeers inspired): everything has volume; a package's
volume = sum of its contents; gas volume depends on pressure. Volume is the **unit of logistics
bandwidth**, and it does real design work:

- It partitions logistics *economically* (see §5) without any hard rules.
- **Volume changes along the production chain** → *where you process* becomes a spatial decision
  (smelt at the mine to ship dense ingots instead of bulky ore).
- *UI note:* present volume as **coarse buckets** (small / medium / large / bulk), not literal
  cm³, to keep it legible rather than fiddly.

---

## 4. Resource taxonomy (the two-source structure)

This is what keeps both the overworld and the dungeon permanently relevant.

| | **Overworld primary** | **Dungeon exotic** |
|---|---|---|
| Role | bulk, the *quantity* game | unlock/gate, the *quality* game |
| Upgrades | many tiers, whole-game | few, tier-specific |
| Demand | **grows without bound** | **fades** once you pass its tier |
| Supply | infinite nodes, spatially scattered | infinite once the dimension is cleared |
| Friction | **logistics** (long trains) | **combat** (conquering the dimension) |

Typical recipe = **lots of (processed) primary + a little exotic.** The exotic *gates* the tier;
the primary *provides the tonnage*. Both pillars appear in nearly every recipe.

Expansion in the overworld is forced by **appetite, not scarcity**: nodes never deplete, but
demand always outruns any single node, so you keep reaching farther and laying more track.
(Purity tiers were considered and **cut** — demand growth alone prevents "build-it-once-and-forget,"
so purity was redundant complexity.)

**Rule of thumb: gate _progression_, never _extraction_.** Better extraction tech should make a
node output *more* (which stresses your logistics — a good problem), never gate *access* to it;
and extractors upgrade **in place** to avoid teardown.

---

## 5. Logistics

A **hybrid** between AE2/RS (instant, addressed, space-collapsing) and belts (physical,
what-goes-where):

- A **declarative command/inventory layer** rides on top of **physical transport.** You can view
  all networked inventories, command movement, output from the network, register recipe
  "promises" (send these inputs here → get this output), and set **keep-N-in-stock** thresholds.
  **The logical layer never teleports** — packages physically travel your belts/trains, so space
  still matters. (Create's package model is the reference spine.)
- **Declarative > imperative.** "Keep 64 steel in stock" states a goal; the network finds the
  steps. This is the correct altitude and removes the need for player scripting.
- **Convenience is an amortization curve.** On-demand/packages: cheap per-unit at low volume,
  expensive per-unit at high (congestion, energy, router caps). Dedicated infra: costly to set up
  once, then cheap per-unit at *any* volume. The crossover decides the regime — **high volume is
  simply inconvenient by either method**, so you build the artery. On-demand stays *available* at
  high volume, just on the wrong side of the curve.
- **Pareto split:** build arteries for the *vital-few* high-volume flows; let on-demand absorb the
  *trivial-many* low-volume varied requests. This is what protects the late-game scarce resource:
  **player attention.**
- **Progressive disclosure = the tech ladder is the tutorial.** Each logistics capability unlocks
  one rung at a time (hand-haul → point-to-point links → network view → promises → stock-keeping),
  each introduced right *after* the player feels the pain it solves.
- **No imperative scripting needed.** Logic primitives are **physical/mechanical first** (a valve
  that stops flow when full). If abstract scripting ever exists, it's an **optional top tier** for
  genuinely *open* control problems only (demand-driven allocation, adaptive modes) — never a gate,
  never for closed "make alloy X" problems.

---

## 6. World structure

- **Overworld** — safe, open, **build-big factory haven.** Blueprint-driven, never threatened.
  Friction is purely logistical (distance, throughput). This is where the bulk primary resources
  and the train/artery game live.
- **The Shaft** — a single **central, diegetic, upgradeable megastructure**: the **spine of
  progression** (the Satisfactory Space-Elevator role). Upgrading the shaft is the **"key"** to
  the next dimension — it replaces boring key-token fetching. Each shaft tier costs a **factory
  milestone component + a node found only at the dangerous frontier of the current dimension**, so
  descending always requires *both* a factory push and a combat push. You can *see* it deepen.
- **Dimensions (the "layers")** — each shaft tier opens a **complete dimension** with a **radial
  difficulty gradient**:
  - *Within a dimension: continuous.* Safe farmable hinterland (infinite basic extraction once
    out-geared) → dangerous frontier (rarer/richer nodes). This built-in risk/reward gradient is
    also the **opt-in renewable combat** for combat-lovers.
  - *Between dimensions: discrete.* Gated by the shaft upgrade above.
  - **The difficulty gradient _is_ the hazard** — no separate environmental-hazard simulation to
    build. Enemy difficulty is the wall.
  - **Stay Cleared:** conquest is one-time. A cleared dimension becomes a safe, infinite resource
    tap; nothing repopulates, nothing to babysit, the factory is never endangered.

---

## 7. Combat

- **Avatar-only — no units.** A powerful, upgradeable **mech** (Prawn-suit inspiration: grapple
  arm, drill arm, thrusters, shields…). Early game = handheld tools/weapons; later = the mech.
  Removing units kills the mass-produce-then-steamroll degenerate strategy and the RTS control
  burden, and gives one clean vehicle that is *both* the skill-expression tool and a factory-output
  sink.
- **Expeditionary, never defensive.** You project force outward into a dimension you *chose* to
  attack. The factory is never on defense.
- **Combat is another process on the automation ladder.** Critically, **gear substitutes for
  skill — and buys down _mechanical_ demand, not just stats** (auto-tracking weapons cut aim
  demand, overshields let you facetank instead of dodge, grapple/thrusters trivialize positioning).
  So a low-skill factory player can over-gear through anything a skilled player does under-geared.
  Neither is hard-walled.
- **Manual play is maximally advantageous** in *efficiency* and *frontier access* (do it now,
  cheap, first) but **never the only path** — automation/gear always eventually reaches any *old*
  frontier. Maximize the efficiency gap; keep the possibility gap at zero.
- Make the manual advantage **qualitative** (do things gear-alone can't), not a flat multiplier.
- **Per-layer variety mapped to mech upgrade slots:** hazard→protection module, terrain→traversal,
  enemy type→weapon/defense, resource→logistics back home. Each dimension ideally introduces a
  *new capability*, not just a stat bump (the grapple arm is the template).
- **Encounters favor _objectives_ over extermination** (reach / hold / destroy), so mobility and
  skilled play matter more than grinding through every enemy.
- **Death = lost run loot/materiel; you keep the mech and upgrades.** Risk is real but recoverable;
  no twitch-punishment for the factory player.
- **Content economics:** combat is a *finite, authored campaign* (its longevity = number of
  distinct dimensions) while the factory is the *infinite* game. Design to that asymmetry. Optional
  opt-in incursions into cleared dimensions give combat-lovers renewable fights without ever forcing
  them or threatening the base.

---

## 8. Direction & guidance (no research currency, no forced quests)

- **Pull, not push.** The **recipe tree _is_ the tech tree**: desirable machines are *visible but
  locked*, gated by **material ladders**. The unlock and the reward are the *same act* (acquiring
  the material), which is why it feels earned rather than assigned. Research-as-currency is dropped
  precisely because it *separates* the unlock from the reward.
- **Keep the tech-tree-as-map, drop the tech-tree-as-currency.** A browsable catalog of what's
  possible (including locked late-game toys) so the player always *covets* something. Stationeers'
  failure mode was an *invisible* horizon.
- **The Shaft is the diegetic megaproject spine** — there's always one big visible thing being
  built/descended (cures "lack of direction" without feeling like a fetch quest, because it's a
  production milestone aligned with the core loop, not a side-errand).
- **The frontier gradient pulls exploration** (Subnautica model: visible danger + gear gating +
  resource breadcrumbs).
- Combined: **spine** (the shaft) answers "where am I going," **recipe ladders** answer "what do I
  do next," **frontier** answers "what's out there."

---

## 9. Core loop

1. **Overworld:** build/expand the factory, grow primary-resource throughput, lay out logistics.
   *(Factory simulates while you delve; pauses when you log off.)*
2. **Forge** the gear/mech upgrade you need (often the very thing that unlocks the next descent).
3. **Descend & pilot:** conquer toward the dimension's frontier, drop extractors on nodes, loot,
   reach the frontier node.
4. **Return & integrate:** bring exotics + the frontier node home, combine with factory production
   to **upgrade the Shaft** → unlock the next dimension.
5. **Repeat.** Each tier raises primary-resource demand, pulling overworld expansion outward.
   *Scarcity migrates;* the bottleneck you fight keeps changing.

**Extraction detail:** drop a forward extractor head on a node; the rest of the machine sits on the
surface and is **powered/fed by inputs** (energy/resources). Once a dimension is cleared, its nodes
are an effectively infinite supply.

---

## 10. Pacing & endgame

- **Master tempo knob:** how much factory work sits between dimension descents.
- **Rhythm:** *demand walls* (forced vertical upgrades on core resources) punctuate *valleys* of
  optional optimization (tradeoff + efficiency upgrades).
- **Endgame:** leaning toward a **satisfying ending** (e.g. reaching the bottom of the shaft / a
  final tier), with optional sandbox continuation. Exact form TBD.

---

## 11. Engine / platform notes

- Voxel, Rust.
- A **strong MP-capable engine** is intended, but **gameplay is designed single-player-shaped** —
  multiplayer does not fork the core design (the factory-player / combat-player division is a happy
  consequence, not a load-bearing requirement).

---

## 12. Anti-goals (check every system against these)

- **The megabase plateau** (blueprint spam → boredom). Defend with efficiency+byproduct *depth* and
  demand-forced *new tiers* — never just bigger copies. Treat "late game must keep posing new design
  problems, not bigger copies" as a standing pillar.
- **Set-and-forget resources.** Demand must always outrun any single solution.
- **Stationeers-grade punishment / obscurity.**
- **Forced fetch-quest feel.** Gates must be production milestones aligned with the core loop, with
  OR-paths inside them (loot *or* extract; skill *or* gear).
- **Combat as a mandatory tax** on factory players / the two pillars feeling orthogonal.

---

## 13. Open questions (to resolve later)

- Setting / fiction.
- Exact ending and what "the bottom" means.
- Depth of the energy model.
- Whether mana ships, and what rule it breaks.
- Ceiling on manual-combat fidelity (how meaty the action layer is).
- Final list of media (which are core vs late).
- Mech control specifics and the deployment/handling feel.

---

## 14. Decided-against (and why)

Recorded so these dead ends aren't re-litigated after a context reset.

- **Factory inside the dungeon / removing the safe overworld.** The factory must stay a safe, open
  home — it is *primary* and *never at risk*. (Raised as an orthogonality fix; rejected.)
- **Base defense / coupling a threat to factory growth (Factorio biters).** The factory is never on
  defense. Dungeon relevance comes from **pull** — a resource the growing factory craves — not from
  danger to the base.
- **Combat via produced units / RTS / an army (deployment caps, supply tails).** Replaced by the
  **avatar-only** mech, which kills the mass-produce-then-steamroll strategy and the RTS control
  burden.
- **Purity tiers — especially tech-gated extraction.** Cut as redundant: demand growth already
  prevents "build-it-once-and-forget," and gating extraction behind tech is a parallel gate that
  kills the spatial decision and invites teardown.
- **Research-as-currency and explicit quest logs.** Direction comes from pull-based recipe ladders +
  the diegetic shaft + the frontier gradient instead.
- **Imperative in-game scripting as a core system.** Declarative stock-keeping + physical logic
  primitives suffice; scripting, if ever, is an optional top tier for open control problems only.
