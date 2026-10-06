# SEMANTIC NeuroOS: "THE COMPUTER THAT REMEMBERS YOU"
## Master Prompt v3: 2-Minute Cinematic Motion-Graphics Film

| | |
|---|---|
| **Runtime** | 2:00 (locked) |
| **Format** | 16:9 · 3840×2160 · 24 fps (vertical 9:16 cut-down in §8) |
| **Shots** | 22, joined by match-cuts and shape morphs, so it plays as one continuous camera journey |
| **Style** | High-end cinematic motion graphics: kinetic typography, smooth shape transitions, 3D elements, seamless camera |
| **Colour** | Each chapter has its own colour world, and the colour shifts with the idea on screen (see §2) |
| **Narration** | About 210 words, told as a story, in a natural conversational voice (see §5) |
| **Version** | v3.0. Replaces the 5-minute v2 script |

> **How to use this document.** Every shot has a `VISUAL` block. To get a shot prompt,
> combine §1 Style Core with that shot's `PALETTE` line and its `VISUAL` block. Record
> the voice first (§5), lay it on the timeline, then cut the picture to it. Add kinetic
> type in post (After Effects, Cavalry, Rive or Blender) and don't let a video model
> render it. AI-generated lettering warps and looks cheap right away.

---

# 1. STYLE CORE
### Prepend this paragraph to every shot prompt.

> Premium cinematic motion graphics in the spirit of high-end title sequences and Apple /
> Nothing / Teenage Engineering product films. Hybrid 3D: glossy glass, brushed metal,
> soft subsurface light, volumetric haze, shallow depth of field, gentle film grain.
> Smooth, weighted motion with no pops, jitter or flicker. Objects **morph** into the next
> object instead of cutting: a ring becomes a portal, particles become a constellation, a
> sphere becomes a laptop. The camera moves continuously: slow dolly-ins, orbital
> arcs, fly-throughs and push-through-object transitions. Rich, saturated, cinematic colour
> grade that changes with the scene. Clean negative space for typography. No people's
> faces in focus, no robots, no circuit-board brains, no green code rain, no spinning
> globes, no stock lens flares.

### 1.1 Camera grammar
- **One continuous journey.** The camera never just cuts. Every transition is one of:
  **push-through** (fly into an object and out the other side into the next scene),
  **match-morph** (the last shape of shot N becomes the first shape of shot N+1),
  **whip-orbit** (fast orbit that settles), or **pull-back reveal**.
- Lenses: 35 mm for the story, 85–100 mm macro for detail, 18–24 mm for the three
  big reveals (shots 6, 11, 22).
- Easing: `cubic-bezier(0.16, 1, 0.3, 1)` on everything. Moves accelerate in and glide out.

### 1.2 Kinetic typography rules
- Display: **Inter Display / Söhne / Neue Haas Grotesk** (300–700). Data: **JetBrains Mono**.
- Type is part of the space. It sits in 3D: wrapped around objects, extruded
  into depth, catching the scene's light, and parallaxing with the camera.
- Entrances vary by meaning. **Mask-reveal** for statements, **per-letter cascade** for
  emotion, **typewriter + cursor** for data, **shatter / dissolve** for loss,
  **magnetic snap** for certainty.
- Key words are colour-keyed to the scene's accent. The rest of the line stays warm white.
- Max 6 words on screen at once, except the answer subtitles in shot 16.

### 1.3 Honesty
Numbers carry a tiny mono tag `TARGET` (engineering target) or `MEASURED`. The end card
carries a one-line in-development disclaimer. Never imply data is uploaded.

---

# 2. COLOUR SCRIPT
The film moves through **six colour worlds**. Each one matches the emotion of its chapter,
and inside each chapter the accent shifts with the key point on screen.

| Ch | Chapter | Time | Colour world | Base | Accents | Feeling |
|---|---|---|---|---|---|---|
| 1 | The Forgetting | 0:00–0:16 | **Glacier** | steel navy `#0E1824` | frost blue `#A9C4D9`, one sodium-orange lamp `#FF8A3D` | cold, lonely |
| 2 | Meaning | 0:16–0:38 | **Aurora** | deep space `#0A0618` | violet `#7B3BFF` → magenta `#FF3DA8` → teal `#19E3D0` | wonder, revelation |
| 3 | The Mind | 0:38–0:58 | **Ember & Brass** → **Emerald Night** | cobalt shadow `#141B4A` | copper `#C8743A`, molten gold `#FFC24B`; then emerald `#2BD98B` on ink `#04140E` | craft, then safety |
| 4 | One Question | 0:58–1:32 | **Midnight & Lamp** | midnight `#0D1530` | tungsten `#FFB86B`, snap cyan `#36E2F0`, each memory its own hue | intimate, human |
| 5 | What It Becomes | 1:32–1:52 | **Four seasons** | changes per vignette | peach dawn, sage kitchen, rose clinic, golden hour | warm, hopeful |
| 6 | Title | 1:52–2:00 | **Spectrum → White** | black | every colour of the film folds into white light | awe, resolve |

**Transition rule:** colour never jump-cuts. It *flows*. A light, a particle or a
shape carries the new colour into frame and spreads it across the scene in 12–24 frames.

---

# 3. THE STORY IN ONE BREATH
Your computer keeps everything and understands nothing, so *you* became its memory. We
turn scattered files into a galaxy of meaning, give it a small mind that lives entirely
inside your laptop, then watch it help one tired engineer at midnight, without a single
byte leaving the machine. Then we glimpse the lives it could change.

**Arc:** frustration → wonder → craft → trust → intimacy → hope.
**The line the film must land:** *"My computer finally knows what I'm doing, and nobody
else ever will."*

---

# 4. SHOT SCRIPT

Each shot lists **TC** (in-point), **DUR** (seconds), **PALETTE**, **VISUAL** (use as
the prompt), **TYPE** (kinetic typography, done in post), **MOVE → NEXT** (camera and transition),
**VO**, and **SFX**.

---

## CHAPTER 1: THE FORGETTING · `0:00–0:16` · Glacier

**1 · `0:00` · 4 s**
- **PALETTE:** steel navy, frost blue, one warm sodium-orange desk lamp glow.
- **VISUAL:** Pure dark. A single word types itself in the centre: `remember`. The
  letters freeze over, frost crawling across them, then hairline cracks. The camera
  dollies back to show the word is on a laptop screen in a cold, blue, empty room at
  night. A small orange lamp glows weakly beside it.
- **TYPE:** `remember` types on → frosts → cracks.
- **MOVE → NEXT:** slow dolly back, then the cracked letters break off the screen and
  fly toward camera.
- **VO:** "You know that feeling…"
- **SFX:** soft room tone, a crisp ice-crack on the letters.

**2 · `0:04` · 4 s**
- **PALETTE:** frost blue, with orange only on the word `where`.
- **VISUAL:** The shards become a **3D storm of fragments** flying past the lens in
  deep parallax: browser tabs, file icons, terminal lines, sticky notes, all grey and
  frosted. The camera pushes forward through them.
- **TYPE:** words flick past at different depths: `41 tabs` · `final_v3_FINAL_actual.pdf`
  · `notes(2).txt` · `where was I?`, the last one glowing orange.
- **MOVE → NEXT:** push forward; the storm parts to reveal a search bar.
- **VO:** "…when you *know* you did the work, but you just can't find it?"
- **SFX:** fluttering paper-and-glass whooshes, panned L/R by depth.

**3 · `0:08` · 4 s**
- **PALETTE:** frost blue; the zero glows a dim, empty white.
- **VISUAL:** A floating 3D search bar. Text types: `that thing about the auth bug`.
  A spinner. The result reads **0**. The zero detaches, extrudes into a thick glass
  **3D ring**, and turns to face camera like a portal.
- **TYPE:** typewriter search text → `0 results`. The `0` becomes the 3D ring.
- **MOVE → NEXT:** **push-through** the hole of the zero.
- **VO:** "Your computer kept every file. It just never understood a single one."
- **SFX:** keystrokes, a flat dead UI tone on `0`, then a deep whoosh through the ring.

**4 · `0:12` · 4 s**
- **PALETTE:** near-black with grey debris and a faint frost rim-light.
- **VISUAL:** Through the ring, a vast void of thousands of grey fragments drifting like
  space debris, **none of them connected**. Lonely, but beautiful.
- **TYPE:** large mask-reveal: `STORAGE` … then a cold strike-through line and
  `≠ MEMORY` slides in beside it.
- **MOVE → NEXT:** slow drift; the camera finds one fragment, and a violet beam of light
  falls on it from above (the colour of the next chapter arrives).
- **VO:** "So *you* became the memory. Every single day."
- **SFX:** low sub-bass swell, then near-silence.

---

## CHAPTER 2: MEANING · `0:16–0:38` · Aurora

**5 · `0:16` · 5 s**
- **PALETTE:** violet beam with magenta highlights, against deep space.
- **VISUAL:** Macro on the lit fragment. A sentence lifts off it:
  `fixed the login timeout`. The letters melt into hundreds of glowing particles that
  swirl, then **snap into a fixed 3D constellation**. The memory has a shape now.
- **TYPE:** sentence in mono → letters dissolve into particles. Small label lands beside
  it: `meaning → a point in space`.
- **MOVE → NEXT:** slow orbit around the constellation; out of focus behind it, other
  fragments start to glow.
- **VO:** "So we tried something different. Instead of storing files… we store *meaning*."
- **SFX:** glass chime as the particles lock; the music begins with a warm 4-note motif.

**6 · `0:21` · 8 s · HERO SHOT**
- **PALETTE:** a travelling wave of violet → magenta → teal, like an aurora rolling
  through the debris.
- **VISUAL:** One continuous 18 mm pull-back. A wave of colour sweeps across the entire
  debris field from shot 4. Every fragment ignites, flies to its place, and **lines of
  light race between related memories**. Clusters pull together (code in teal, notes in
  magenta, calendar in violet), and the debris becomes a vast, breathing **3D lattice
  of light**, like a living galaxy. It pulses once, like a breath.
- **TYPE:** none. Let the image carry it.
- **MOVE → NEXT:** the pull-back slows into a 180° orbit, then dives *into* the lattice.
- **VO:** "Every moment of your work becomes a point of light, and things that *mean*
  the same thing end up close together."
- **SFX:** huge reverse-swell resolving into the full 4-note motif.

**7 · `0:29` · 5 s**
- **PALETTE:** teal dominant, with magenta nodes brightening as they pass the lens.
- **VISUAL:** Fly-through between the nodes with heavy parallax. Glass nodes refract the
  light around them. Thin threads of light connect them in real 3D space.
- **TYPE:** a sentence **wraps along the curve of a light-thread** and travels with
  it: `similar ideas live together` (`together` in magenta).
- **MOVE → NEXT:** the whole lattice **contracts** toward the lens and collapses into
  one perfect glowing sphere.
- **VO:** —
- **SFX:** soft proximity swells as nodes pass camera.

**8 · `0:34` · 4 s**
- **PALETTE:** aurora sphere, its surface swirling violet, magenta and teal.
- **VISUAL:** The sphere hangs in space, the whole galaxy compressed inside it and
  swirling like marble.
- **TYPE:** big mask-reveal: `THE SEMANTIC MATRIX`. Smaller beneath: `storage organised
  by meaning`.
- **MOVE → NEXT:** the sphere drops downward out of frame. The camera tilts to follow it,
  and copper light rises from below (next chapter).
- **VO:** "We call it the Semantic Matrix."
- **SFX:** chord sustains, then a soft falling tone.

---

## CHAPTER 3: THE MIND · `0:38–0:58` · Ember & Brass → Emerald Night

**9 · `0:38` · 4 s**
- **PALETTE:** copper and molten gold on cobalt shadow.
- **VISUAL:** The sphere lands in the centre of a **laptop seen from directly above**,
  drawn in dark brushed metal. On impact, molten-gold veins race outward through the
  keyboard, hinge and screen, like a **nervous system lighting up**.
- **TYPE:** `NeuroOS` assembles from the gold veins themselves. Beneath it:
  `a mind that lives inside your laptop`.
- **MOVE → NEXT:** the camera rises; the laptop **unfolds/explodes** into separate 3D parts.
- **VO:** "Then we gave it a mind."
- **SFX:** a deep heartbeat thud on impact, then light racing along fibre.

**10 · `0:42` · 10 s · The seven organs**
- **PALETTE:** copper base, but **each organ ignites in its own colour** as the camera
  reaches it.
- **VISUAL:** Seven glass-and-brass 3D forms float in an arc. The camera does a smooth
  continuous **whip-orbit**, settling on each for ~1.4 s as it lights up:
  1. **SENSE** (cyan): radar ripples catch a window opening.
  2. **HEAR** (coral): a sound wave enters and leaves as a voice bloom.
  3. **REMEMBER** (aurora violet): a miniature of the Semantic Matrix, breathing.
  4. **THINK** (gold): a crystal of tiny cubes flipping `−1 · 0 · +1` in waves.
  5. **UNDERSTAND** (lime): a mechanical arm reaches back along a timeline.
  6. **GUARD** (emerald): an iris-gate of rotating rings.
  7. **REACH OUT** (orange): smaller and set apart, on a single thin leash of light,
     facing outward into the dark.
- **TYPE:** each organ's single word **snaps magnetically** onto its surface in its own
  colour, then fades as the camera moves on.
- **MOVE → NEXT:** the orbit completes; organs 1–6 are pulled together toward centre.
- **VO:** "Something that can see what you're doing… hear you… remember… think…
  understand what you mean… and *ask* before it acts. Only one small part is ever
  allowed online, and only when you say so."
- **SFX:** a distinct tactile sound per organ (ping, breath, thud, abacus click, servo,
  iris, taut cable).

**11 · `0:52` · 6 s · THE SEAL**
- **PALETTE:** shifts to **Emerald Night**: emerald light on near-black ink.
- **VISUAL:** 24 mm pull-back. An emerald **hexagonal shell morphs closed** around the
  six organs, panel by panel, like petals locking shut, with a visible shockwave. Outside,
  the cold dark "internet" presses in. A single orange packet flies at the shell, hits
  emerald, and **evaporates** into sparks. Only the leashed orange organ stays outside.
- **TYPE:** `ZERO EGRESS` with a heavy mask-reveal; mono counter `0 bytes sent` holding at
  zero, tag `ENFORCED BY THE OS`.
- **MOVE → NEXT:** the camera **pushes through** the emerald shell. On the other side,
  warm tungsten light: a real desk at night.
- **VO:** "Everything else? Sealed inside your laptop."
- **SFX:** a huge satisfying magnetic *thunk*, a ringing emerald tone, a tiny dry
  fizz as the packet dies.

---

## CHAPTER 4: ONE QUESTION · `0:58–1:32` · Midnight & Lamp

**12 · `0:58` · 4 s**
- **PALETTE:** midnight navy room, warm tungsten desk lamp, cold coffee.
- **VISUAL:** Photoreal, cosy, lived-in desk at night: two monitors, code editor on
  `auth.rs`, mechanical keyboard, notebook. A person (Aarav) leans back in soft-focus
  silhouette, rubbing his eyes. Face never in focus.
- **TYPE:** mono slate typing in the corner: `Tuesday · 23:41 · day 4 of a login bug`.
- **MOVE → NEXT:** slow push-in toward the laptop microphone.
- **VO:** "Here's what that actually feels like."
- **SFX:** room tone, a fan, a tired exhale.

**13 · `1:02` · 5 s**
- **PALETTE:** tungsten amber waveform; one word in hot pink.
- **VISUAL:** He speaks. His voice becomes a **flowing amber ribbon** across the frame,
  and the words grow out of it as kinetic type. The word **"this"** glows hot pink and
  pulses, clearly unresolved.
- **TYPE:** `"Hey Jarvis, what was I doing with this bug last Thursday?"`
  with `this` pulsing pink.
- **MOVE → NEXT:** the camera locks onto `this`, then whip-pans with it.
- **VO:** *(diegetic: Aarav's line)*
- **SFX:** his voice, close and natural; a soft wake chime.

**14 · `1:07` · 6 s · "THIS" SNAP**
- **PALETTE:** pink word travels into a field of **snap cyan**.
- **VISUAL:** The word **"this"** flies across frame onto a glowing horizontal timeline.
  Two brackets **slam shut** around the moment he spoke (±1.5 s). Inside the brackets is
  the window that was on his screen: `auth.rs: login timeout`. "this" docks into it
  with a **magnetic click**, and pink turns cyan. The question is complete.
- **TYPE:** `"this" = what was on your screen` mask-reveals beneath.
- **MOVE → NEXT:** the completed sentence compresses into a bright cyan **flare** and
  launches forward.
- **VO:** "It knows what *'this'* means, because it saw what was on your screen."
- **SFX:** whip whoosh, camera-shutter snap, magnetic clack (the most satisfying
  sound in the film).

**15 · `1:13` · 6 s**
- **PALETTE:** the aurora lattice returns; **five memories each in their own colour**.
- **VISUAL:** The flare fires into the Semantic Matrix and the camera follows it in at
  speed. Nodes flare as it passes. Then **five nodes, and only five**, fly out toward
  the camera, each a different hue:
  - git commit: **cobalt**
  - note: **sunflower yellow**
  - failing test: **crimson**
  - calendar: **lavender**
  - web page: **orange**, and a cage of light snaps shut around it, flagged *untrusted*.
- **TYPE:** each label types on beside its node. Big number centre:
  `5 of 100,000 · ~12 ms` with tag `TARGET`.
- **MOVE → NEXT:** the five nodes fly back toward the desk and orbit the laptop.
- **VO:** "Five moments, out of a hundred thousand, in the blink of an eye."
- **SFX:** flare launch, five bright arrival notes, a cold metallic cage-click on the
  orange one.

**16 · `1:19` · 9 s · THE ANSWER**
- **PALETTE:** back to the warm tungsten desk, the five coloured nodes orbiting softly.
- **VISUAL:** The laptop answers in a calm voice. **As each sentence is spoken, its
  source memory glides in beside it** like a footnote you can see. The orange web
  memory stays caged. Aarav slowly sits forward.
- **TYPE:** subtitles, each line paired with its coloured node:
  ```
  Last Thursday you traced it to the token refresh path.   ● cobalt
  There's a partial fix committed —                         ● sunflower
  but the expired-session test is still failing.            ● crimson
  ```
- **MOVE → NEXT:** slow push past his shoulder to the second monitor.
- **VO:** *(diegetic: NeuroOS answer, then Aarav, quietly: "…right.")*
- **SFX:** the 4-note motif returns, very quiet, under the answer.

**17 · `1:28` · 4 s · THE PROOF**
- **PALETTE:** emerald line on midnight.
- **VISUAL:** A network monitor. A **perfectly flat emerald line**. The counter reads
  `0 bytes`. Nothing moves except dust in the screen-light. Hold the stillness.
- **TYPE:** `NETWORK EGRESS · 0 bytes`
- **MOVE → NEXT:** the emerald line **morphs** into a warm horizon line: sunrise.
- **VO:** "And nothing left the machine. Not one byte."
- **SFX:** near-silence; a single slow heartbeat.

---

## CHAPTER 5: WHAT IT BECOMES · `1:32–1:52` · Four Seasons

*Each vignette is its own colour world. The lattice is faintly present in every room,
as if the space itself remembers.*

**18 · `1:32` · 5 s · PEACH DAWN: The Return**
- **VISUAL:** Sunlit study, peach and cream light. A researcher opens a laptop after
  weeks away. Glowing particles, like dandelion seeds in reverse, float up and
  **rebuild their train of thought** in the air around them.
- **TYPE:** `"Welcome back. You were three steps from the answer."`
- **MOVE → NEXT:** a single seed drifts across frame into…
- **VO:** "Imagine coming back after a month, and it remembers exactly where you stopped."

**19 · `1:37` · 5 s · SAGE KITCHEN: The Simple Machine**
- **VISUAL:** Soft sage-green kitchen. An older woman talks to her laptop. Menus,
  folders and toolbars **peel away like petals** until only a calm surface and a voice
  remain.
- **TYPE:** `no menus · no manuals · just talk`
- **MOVE → NEXT:** a petal turns rose-coloured and drifts into…
- **VO:** "A computer your grandmother can just… talk to."

**20 · `1:42` · 5 s · ROSE CLINIC: The Private Practice**
- **VISUAL:** A doctor's room in dusky rose and clean white. Sensitive notes on screen.
  The **emerald shell from shot 11** shimmers faintly around the room. A stamp seats
  into frame.
- **TYPE:** `private by design · not by promise`
- **MOVE → NEXT:** the shell glints gold and becomes…
- **VO:** "An assistant a doctor can trust, because it physically *can't* tell anyone."

**21 · `1:47` · 5 s · GOLDEN HOUR: The Long Memory**
- **VISUAL:** Golden-hour light. A lattice **grows in rings like a tree** across ten
  years, with year-markers streaming past. A hand reaches in, selects one region, and it
  vanishes cleanly.
- **TYPE:** `ten years · yours to keep · or erase`
- **MOVE → NEXT:** the camera pulls back and every colour in the frame begins to stream
  toward one point.
- **VO:** "Ten years of your work. Yours to keep… or to let go."

---

## CHAPTER 6: TITLE · `1:52–2:00` · Spectrum → White

**22 · `1:52` · 8 s**
- **PALETTE:** every colour from the film (frost, aurora, copper, emerald, tungsten,
  peach, sage, rose, gold) streams in as ribbons and **folds into one white point**.
- **VISUAL:** 18 mm pull-back into black. The ribbons spiral inward and converge into a
  single steady point of white light. It **pulses once**, like a heartbeat. The title
  assembles around it.
- **TYPE:**
  ```
  SEMANTIC NeuroOS
  Your machine. Your memory. Nothing leaves.
  ```
  small mono: `on-device · zero-egress · in development`
  lower-third, 60% opacity: *In active development. Figures shown are engineering targets.*
- **VO:** "Semantic NeuroOS. Your machine. Your memory. Nothing leaves."
- **SFX:** music resolves; one heartbeat; the final piano note; black.

---

# 5. VOICE & NARRATION

## 5.1 Voice direction (what went wrong last time, and the fix)
The narration should sound like **a person telling a friend a story**, not a
trailer voice, a news anchor or an AI assistant.

- **Tone:** warm, curious, slightly amused, a little quiet. Imagine talking across a
  kitchen table at night, close to the mic.
- **Pace:** *uneven on purpose.* Slow down on emotional lines, speed up a little on
  lists. Average about 140 words per minute, never a constant rhythm.
- **Breath:** keep natural breaths in. Don't edit them all out.
- **Smile:** a slight audible smile on shots 14, 16 and 18.
- **Avoid:** announcer cadence, a rising "sales" tone at line ends, over-enunciation,
  and the same pitch pattern on every sentence.

## 5.2 If using text-to-speech (ElevenLabs or similar)
- Pick a voice labelled **conversational / narrative / storytelling**, not
  "news", "commercial" or "announcer". Audition 3–4 voices on the shot-10 line, since it's
  the hardest one.
- Suggested ElevenLabs settings: **Stability 30–40 %** (lower = more expressive),
  **Similarity 70–80 %**, **Style 15–30 %**, Speaker Boost on. With a model that
  supports audio tags (e.g. v3), use them lightly: `[softly]`, `[pause]`, `[warmly]`.
- **Generate line by line, not the whole script in one pass.** Make 3 takes of each
  line and keep the most human one.
- Control rhythm with punctuation: `…` for a soft hesitation, `—` for a turn, a comma
  for a short breath, and a full stop for a real stop.
- In post: add a whisper of room tone under the voice, use light compression (not
  broadcast-heavy), cut a little 200–400 Hz mud and roll off below 80 Hz.
- **The best option, if you can:** record a real person on a decent USB mic in a
  carpeted room. Even one take sounds more natural than any TTS.

## 5.3 Narration script (record first)
`…` = soft hesitation · `//` = breath · `///` = longer pause (½–1 s)

> **[Ch 1: The Forgetting]**
> You know that feeling… // when you *know* you did the work, but you just can't find
> it? /// Your computer kept every file. // It just never understood a single one. ///
> So *you* became the memory. // Every single day. ///
>
> **[Ch 2: Meaning]**
> So we tried something different. // Instead of storing files… we store *meaning*. ///
> Every moment of your work becomes a point of light, // and things that *mean* the same
> thing end up close together. /// We call it the Semantic Matrix. ///
>
> **[Ch 3: The Mind]**
> Then we gave it a mind. /// Something that can see what you're doing… hear you…
> remember… think… understand what you mean… // and *ask* before it acts. // Only one
> small part is ever allowed online, and only when you say so. /// Everything else? //
> Sealed inside your laptop. ///
>
> **[Ch 4: One Question]**
> Here's what that actually feels like. ///
> *[Aarav's line]*
> It knows what *"this"* means, // because it saw what was on your screen. /// Five
> moments, out of a hundred thousand, // in the blink of an eye. ///
> *[NeuroOS answers · Aarav: "…right."]*
> And nothing left the machine. // Not one byte. ///
>
> **[Ch 5: What It Becomes]**
> Imagine coming back after a month, // and it remembers exactly where you stopped. //
> A computer your grandmother can just… talk to. // An assistant a doctor can trust,
> because it physically *can't* tell anyone. // Ten years of your work. // Yours to
> keep… or to let go. ///
>
> **[Ch 6: Title]**
> Semantic NeuroOS. // Your machine. // Your memory. // Nothing leaves.

## 5.4 Dialogue (separate voices)
| Shot | Who | Line | Direction |
|---|---|---|---|
| 13 | Aarav | "Hey Jarvis, what was I doing with this bug last Thursday?" | tired, casual, mid-yawn energy |
| 16 | NeuroOS | "Last Thursday you traced it to the token refresh path. There's a partial fix committed, but the expired-session test is still failing." | calm, friendly, unhurried, like a helpful colleague and not a robot |
| 16 | Aarav | "…right." | barely audible, relieved |
| 18 | NeuroOS | "Welcome back. You were three steps from the answer." | gentle, warm |

Use a **different voice** for NeuroOS than for the narrator, so the viewer always knows
who is speaking.

## 5.5 Music
Light and supportive, never heavy. A felt-piano 4-note motif (introduced at shot 5,
returning at 6, 16 and 22) over a soft evolving pad and a subtle pulse. Duck the music
6–8 dB under every spoken line. Hold near-silence at shot 4 and shot 17. Recommended
references: Ólafur Arnalds, Jon Hopkins (soft), Nils Frahm.

---

# 6. TIMING SHEET

| Ch | Chapter | In | Out | Dur | Shots | Colour world |
|---|---|---|---|---|---|---|
| 1 | The Forgetting | 0:00 | 0:16 | 16 s | 1–4 | Glacier |
| 2 | Meaning | 0:16 | 0:38 | 22 s | 5–8 | Aurora |
| 3 | The Mind | 0:38 | 0:58 | 20 s | 9–11 | Ember & Brass → Emerald Night |
| 4 | One Question | 0:58 | 1:32 | 34 s | 12–17 | Midnight & Lamp |
| 5 | What It Becomes | 1:32 | 1:52 | 20 s | 18–21 | Peach · Sage · Rose · Gold |
| 6 | Title | 1:52 | 2:00 | 8 s | 22 | Spectrum → White |
| | **Total** | | **2:00** | **120 s** | **22** | |

**The four shots to protect if time is short:** 6 (debris → lattice), 11 (the seal),
14 ("this" snap), 22 (colours fold into white).

---

# 7. IF GENERATING WITH AN AI VIDEO TOOL (Runway, Kling, Veo, Sora, Pika, etc.)

1. **Prompt formula for every shot:**
   `[§1 Style Core] + "Colour palette: " + [PALETTE] + [VISUAL] + "Camera: " + [MOVE]`.
2. **Generate in 4–8 s clips** at the stated duration. Use **image-to-video** with
   the *last frame of the previous shot* as the first frame of the next, which is what
   makes the transitions feel seamless.
3. **Generate the lattice (shot 6) first** and reuse its frames as references in
   shots 7, 8, 10 and 15 so it stays the same object.
4. **Never ask the model to render text.** Add every `TYPE` block as kinetic typography
   in post (After Effects / Cavalry / Blender), tracked to the camera so it sits in 3D.
5. **Hand-animate rather than generate** shot 10's organ labels, shot 14's brackets and
   timeline, shot 15's number, and shot 22's title. Precision matters more there.
6. **Grade per chapter** to the §2 colour script. Generated clips rarely arrive on
   palette, so match them in the grade and use the colour-carrying transition elements
   to blend between worlds.
7. **Voice first.** Lay down §5 narration and dialogue, then cut the clips to it.

---

# 8. DELIVERABLES

| # | Deliverable | Spec |
|---|---|---|
| 1 | Master | 2:00 · 4K · ProRes 422 HQ · stereo |
| 2 | Web cut | 2:00 · 1080p H.264 · stereo · burned-in captions variant |
| 3 | Vertical cut | 0:45 · 9:16 · shots 3 → 6 → 11 → 13 → 14 → 16 → 22 |
| 4 | Teaser loop | 0:10 · silent · shot 6 lattice assembly, seamless loop |
| 5 | Hero stills | shots 6, 11, 14, 22 as clean 4K plates without type |
| 6 | Captions | `.srt` |
