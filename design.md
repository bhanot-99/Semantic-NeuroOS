# design.md — Design System

**Document version:** 1.0 · **Date:** 2026-09-26 · **Status:** Baseline
**Theme name:** **"Synapse"** — calm, dark-first, technical, trustworthy.
**Scope:** every surface a human sees or hears from Semantic-NeuroOS.

---

## 1. Design Principles

| # | Principle | Meaning |
| :--- | :--- | :--- |
| DP-1 | **Quiet by default** | NeuroOS lives in the background. It speaks briefly, shows UI only when it needs a decision, and never animates for decoration. |
| DP-2 | **Safety is visible** | Tier and taint are always shown with colour **and** text **and** an icon. Colour alone never carries meaning. |
| DP-3 | **Honest and specific** | Dialogs say exactly what will happen, with which data, and why confirmation is needed. No vague "Allow access?". |
| DP-4 | **Offline, always** | Every asset (fonts, scripts, icons) is bundled. No surface ever makes a network request. |
| DP-5 | **Native where it matters** | The confirmation dialog respects the desktop's system font and light/dark preference. Custom styling is limited to semantic colours. |
| DP-6 | **Accessible** | WCAG 2.2 AA contrast minimum, full keyboard use, screen-reader labels, reduced-motion support. |

---

## 2. Surfaces Inventory

| Surface | Technology | Owner | Phase |
| :--- | :--- | :--- | :--- |
| S-1 Voice (preambles, answers, errors) | Kokoro-82M, one voice | C2 / C5 | 6 |
| S-2 Confirmation dialog `neuroos-confirm` | GTK4 (Zenity fallback) | C6 | 7 |
| S-3 Knowledge graph view `graph_view.html` | Static HTML + inline D3 v7 + inline fonts | C5 | 5 |
| S-4 Operator CLI `neuroosctl` | Terminal (ANSI, 256/truecolor, `NO_COLOR`) | CLI | 1+ |
| S-5 Reports and docs | Markdown | all | all |
| S-6 HUD overlay (post-v1.0) | TBD | — | backlog |

---

## 3. Colour System

All colours are defined once as design tokens in `ui/tokens/tokens.json` and generated into CSS variables (graph view), GTK CSS (dialog) and an ANSI palette (CLI). Never hard-code a hex value outside the token file.

### 3.1 Core palette — Dark theme (default)

| Token | Hex | Use | Contrast vs `bg.canvas` |
| :--- | :--- | :--- | :--- |
| `bg.canvas` | `#0E1116` | Page / window background | — |
| `bg.surface` | `#161B22` | Cards, panels, dialog body | — |
| `bg.raised` | `#1F2630` | Popovers, tooltips, hovered rows | — |
| `border.subtle` | `#2A3340` | Dividers, card outlines | — |
| `border.strong` | `#3B4655` | Input outlines, focus-adjacent borders | — |
| `text.primary` | `#E6EDF3` | Body and headings | 16.0 : 1 |
| `text.secondary` | `#9BA7B4` | Labels, metadata | 7.7 : 1 |
| `text.muted` | `#8A95A3` | Hints, timestamps (≥ 5.0 : 1 on every dark background) | 6.2 : 1 |
| `accent.primary` | `#2DD4BF` | "Synapse teal": brand, primary buttons, focus ring, links | 10.2 : 1 |
| `accent.primary.fg` | `#0E1116` | Text on teal buttons | 10.2 : 1 |
| `accent.secondary` | `#A78BFA` | Secondary highlights, knowledge family | 6.9 : 1 |

### 3.2 Core palette — Light theme

| Token | Hex | Contrast vs `#FFFFFF` |
| :--- | :--- | :--- |
| `bg.canvas` | `#FFFFFF` | — |
| `bg.surface` | `#F6F8FA` | — |
| `bg.raised` | `#FFFFFF` (+ shadow) | — |
| `border.subtle` | `#D8DEE4` | — |
| `border.strong` | `#AFB8C1` | — |
| `text.primary` | `#1F2328` | 15.8 : 1 |
| `text.secondary` | `#59636E` | 6.1 : 1 |
| `text.muted` | `#636C76` | 5.3 : 1 (5.0 on surface) |
| `accent.primary` | `#0F766E` | 5.5 : 1 |
| `accent.primary.fg` | `#FFFFFF` | 5.5 : 1 on teal |
| `accent.secondary` | `#6D28D9` | 7.1 : 1 |

### 3.3 Semantic colours — Safety tiers and taint

These colours carry security meaning. They are **reserved**: never use them for decoration.

| Token | Dark | Light | Icon | Label text | Meaning |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `tier.safe` | `#3FB950` | `#1A7F37` | shield-check | `SAFE` | Runs automatically |
| `tier.review` | `#D29922` | `#9A6700` | shield-alert | `NEEDS YOUR OK` | Requires confirmation |
| `tier.dangerous` | `#F85149` | `#CF222E` | shield-x | `DANGEROUS` | Confirmation + typed phrase |
| `tier.dangerous.solid` | `#DA3633` (white text 4.6 : 1) | `#CF222E` | — | — | Filled danger button |
| `taint.external` | `#DB61A2` | `#BF3989` | globe-alert | `FROM THE WEB` | Content came from outside the device |
| `status.info` | `#58A6FF` | `#0969DA` | info | — | Neutral information |
| `status.ok` | = `tier.safe` | = `tier.safe` | check | `OK` | Health OK |
| `status.degraded` | `#F0883E` | `#BC4C00` | alert-triangle | `DEGRADED` | Health degraded (distinct from REVIEW amber) |
| `status.down` | = `tier.dangerous` | = `tier.dangerous` | x-circle | `DOWN` | Health down |

All semantic colours meet ≥ 4.5 : 1 against `bg.canvas` and `bg.surface` in their theme.

### 3.4 Data visualisation palette — Domain families

13 domains are too many for distinguishable colours. Colour encodes the **family** (5 hues); shape encodes the **domain** within a family.

| Family | Dark | Light | Domains (shape) |
| :--- | :--- | :--- | :--- |
| Attention | `#58A6FF` | `#0969DA` | window_focus (●), app_lifecycle (■), idle_presence (▲) |
| Work | `#2DD4BF` | `#0F766E` | process_activity (●), build_job (■), git_activity (◆) |
| Knowledge | `#A78BFA` | `#6D28D9` | notes (●), calendar (■), voice_interaction (▲) |
| System | `#8B949E` | `#57606A` | media_playback (●), system_resource (■), assistant_actions (◆) |
| External | `#DB61A2` | `#BF3989` | external_documents (◆ with dashed outline) |

Edges: `border.strong` at 60% opacity; hypothesis edges (not yet reinforced) are dashed; edge width ∝ weight (1–4 px).

### 3.5 Theme selection

- Graph view: follows `prefers-color-scheme`, with a manual toggle stored in `localStorage` (offline, local only).
- Dialog: follows the desktop's GTK/COSMIC dark preference.
- CLI: assumes a dark terminal; `--color=never` or `NO_COLOR` disables colour; `--theme=light` swaps to light ANSI values.

---

## 4. Typography

### 4.1 Font families

| Role | Font | Fallback stack | Licence | Where |
| :--- | :--- | :--- | :--- | :--- |
| UI / text | **Inter** (variable, v4) | `"Inter", "Open Sans", "Noto Sans", system-ui, sans-serif` | SIL OFL 1.1 | Graph view, HUD |
| Monospace / data | **JetBrains Mono** (variable) | `"JetBrains Mono", "Noto Sans Mono", "DejaVu Sans Mono", ui-monospace, monospace` | SIL OFL 1.1 | Graph view metadata, code, IDs, hashes |
| Dialog | **System UI font** (COSMIC/GTK default) | GTK default | — | `neuroos-confirm` (DP-5) |
| CLI | The user's terminal font | — | — | `neuroosctl` |

Font files are vendored in `ui/graph-view/fonts/` as subset WOFF2 (Latin + Latin-1 Supplement + punctuation + arrows) and **inlined as base64** into `graph_view.html` at build time (DP-4). Inter features enabled: `"cv11", "ss01", "tnum"` for numbers in tables and metrics.

### 4.2 Type scale

Base size **14 px** (dense tool UI), ratio **1.2 (minor third)**, rounded to whole pixels. Line heights snap to the 4 px grid.

| Token | Size / line height | Weight | Letter spacing | Use |
| :--- | :--- | :--- | :--- | :--- |
| `type.display` | 36 / 44 px | 600 | −0.02 em | Graph view title only |
| `type.h1` | 30 / 36 px | 600 | −0.015 em | Page heading |
| `type.h2` | 24 / 32 px | 600 | −0.01 em | Section heading, dialog title (graph view) |
| `type.h3` | 20 / 28 px | 600 | −0.005 em | Panel heading |
| `type.h4` | 16 / 24 px | 600 | 0 | Card title, dialog action name |
| `type.body` | 14 / 20 px | 400 | 0 | Default text |
| `type.body.strong` | 14 / 20 px | 600 | 0 | Emphasis, labels |
| `type.small` | 13 / 18 px | 400 | 0 | Secondary metadata |
| `type.caption` | 12 / 16 px | 500 | 0.01 em | Captions, legend, timestamps |
| `type.overline` | 11 / 16 px | 600 | 0.08 em, UPPERCASE | Tier and taint badges |
| `type.mono` | 13 / 20 px | 400 | 0 | IDs, hashes, paths, code |
| `type.mono.small` | 12 / 16 px | 400 | 0 | Dense tables |

### 4.3 Typography rules

1. Sentence case for all headings, buttons and labels. UPPERCASE only for `type.overline` badges.
2. Maximum line length 72 characters for prose.
3. Numbers in tables and metrics use tabular figures (`font-variant-numeric: tabular-nums`).
4. Hashes, action IDs and paths are always monospace and truncated in the middle (`sha256:3fa1…9c2e`) with the full value in a tooltip / `--verbose`.
5. Minimum rendered text size 12 px (11 px only for overline badges with 600 weight).
6. Never use more than two weights on one surface (400 and 600; 500 only for captions).
7. Units always have a space and use SI symbols: `13 ms`, `205 MiB`, `24 kHz`.

---

## 5. Layout, Spacing and Shape

| Token | Value |
| :--- | :--- |
| Base grid | 4 px |
| Spacing scale | `space.1` 4 · `space.2` 8 · `space.3` 12 · `space.4` 16 · `space.5` 20 · `space.6` 24 · `space.8` 32 · `space.10` 40 · `space.12` 48 px |
| Radius | `radius.sm` 4 px (badges, inputs) · `radius.md` 8 px (cards, buttons) · `radius.lg` 12 px (dialogs, panels) · `radius.full` (pills) |
| Border width | 1 px default; 2 px focus ring (`accent.primary`, 2 px offset) |
| Elevation (dark) | `shadow.1` `0 1px 2px rgba(0,0,0,.4)` · `shadow.2` `0 4px 12px rgba(0,0,0,.45)` · `shadow.3` `0 12px 32px rgba(0,0,0,.55)` |
| Elevation (light) | `shadow.1` `0 1px 2px rgba(31,35,40,.08)` · `shadow.2` `0 4px 12px rgba(31,35,40,.12)` · `shadow.3` `0 12px 32px rgba(31,35,40,.16)` |
| Icons | Lucide (ISC licence), vendored SVG, 16 px in text, 20 px in buttons, 1.75 px stroke |
| Motion | `motion.fast` 120 ms, `motion.base` 200 ms, easing `cubic-bezier(0.2, 0, 0, 1)`. Respect `prefers-reduced-motion: reduce` (no transitions, graph layout pre-settled). |

---

## 6. Knowledge Graph View (S-3)

**File:** `~/.local/share/neuroos/graph_view.html`, generated from `ui/graph-view/template.html`.

### 6.1 Layout

```
┌──────────────────────────────────────────────────────────────────────────┐
│ NeuroOS · Knowledge graph        [Search nodes…]   [Family ▾] [◐ Theme]  │  header 56 px, bg.surface
├───────────────┬──────────────────────────────────────────────────────────┤
│ LEGEND        │                                                          │
│ ● Attention   │                force-directed canvas                     │
│ ● Work        │                (bg.canvas, SVG ≤ 2k nodes,               │
│ ● Knowledge   │                 Canvas2D above 2k)                       │
│ ● System      │                                                          │
│ ◆ External    │                                                          │
│ ─ ─ hypothesis│                                                          │
│               │                                ┌───────────────────────┐ │
│ STATS         │                                │ build_job             │ │
│ Nodes  1,284  │                                │ cargo build (err 104) │ │
│ Edges  3,902  │                                │ ◆ FROM THE WEB        │ │
│ Generated     │                                │ last seen 10:15 UTC   │ │
│ 10:20 UTC     │                                └───────────────────────┘ │
├───────────────┴──────────────────────────────────────────────────────────┤
│ Generated locally · no network requests · snapshot of 2026-09-26 10:20Z  │  footer, type.caption, text.muted
└──────────────────────────────────────────────────────────────────────────┘
  sidebar 240 px                                           detail card 320 px
```

### 6.2 Rules

- Node radius 4–14 px by degree (sqrt scale). Selected node: 2 px `accent.primary` ring; neighbours at full opacity, others at 25%.
- Tainted nodes always show the ◆ shape plus a `FROM THE WEB` badge in the detail card.
- Labels appear only at zoom ≥ 1.5× or on hover, in `type.caption`, with a `bg.canvas` halo for legibility.
- Keyboard: `/` focuses search, arrow keys move between neighbours, `Esc` clears selection, `+`/`-` zoom.
- Every interactive element has an accessible name; the node list is also available as a hidden, screen-reader-navigable table.
- The page must make **zero network requests** (verified in Phase 5 tests).

---

## 7. Confirmation Dialog (S-2) — `neuroos-confirm`

The most important UI in the product. It must be impossible to misunderstand.

### 7.1 Anatomy

```
┌──────────────────────────────────────────────────────────┐
│  🛡  NEEDS YOUR OK                                        │  tier badge (overline, tier colour + icon)
│                                                          │
│  Read your calendar                                      │  action title, h4 (plain-language skill name)
│  NeuroOS wants to search events matching "api-docs".     │  body: exactly what will happen
│                                                          │
│  ┌────────────────────────────────────────────────────┐  │
│  │ ◆ FROM THE WEB                                     │  │  taint panel (only when tainted),
│  │ This request was influenced by content fetched     │  │  taint.external left border 4 px
│  │ from example.com. That content could be trying     │  │
│  │ to trick the assistant.                            │  │
│  └────────────────────────────────────────────────────┘  │
│                                                          │
│  Details ▸                                               │  expander: capability id, args (mono),
│                                                          │  action id, expiry countdown
│                       [ Deny ]   [ Allow once ]          │  Deny = default focus
│  Expires in 1:54                                         │  caption, text.muted
└──────────────────────────────────────────────────────────┘
  width 440 px · radius.lg · padding space.6 · modal, always on top
```

### 7.2 Behaviour rules

1. **Default focus is Deny.** `Enter` on open never approves. `Esc` = Deny.
2. The approve button says what it does: "Allow once", never "OK" or "Yes".
3. DANGEROUS tier: the approve button is `tier.dangerous.solid`, disabled until the user types the phrase shown (e.g. `run shell command`).
4. Countdown shows the 120 s expiry; at 0 the dialog closes and the action is recorded as `EXPIRED`.
5. Window `app_id` is `org.neuroos.Confirm` (excluded from C1/C3 self-observation).
6. No remote content, images or links in the dialog. URLs are shown as plain text, domain first, in monospace.
7. Screen readers announce: tier, action title, taint warning, then buttons.
8. The dialog never shows raw prompt text or model output — only the structured action.

---

## 8. CLI Design (S-4) — `neuroosctl`

### 8.1 Output conventions

| Element | Style |
| :--- | :--- |
| Headers | bold, `text.primary` |
| Labels | `text.secondary` |
| Values | default terminal foreground |
| IDs / hashes / paths | dim, middle-truncated |
| Status words | coloured **and** symbol: `✔ OK`, `▲ DEGRADED`, `✖ DOWN`, `… UNKNOWN` |
| Tier / taint | `[SAFE]`, `[REVIEW]`, `[DANGEROUS]`, `[WEB]` in tier colours |
| Errors | `error:` prefix in `tier.dangerous`, one-line cause, then `hint:` line |
| Streaming answers (`ask`) | Plain text streamed as tokens arrive; sources listed after, dimmed |

### 8.2 Example — `neuroosctl status`

```
NeuroOS  v1.0.0  ·  healthy 7/8  ·  total RSS 2,237 / 2,340 MiB

COMPONENT            STATUS       RSS / BUDGET        P99        UPTIME
neuroos-healthd      ✔ OK          12 /   15 MiB      —         3d 04h
neuroos-monitor      ✔ OK          21 /   25 MiB      0.9 ms    3d 04h
neuroos-voice        ✔ OK         381 /  405 MiB     41 ms      3d 04h
neuroos-storage      ✔ OK         188 /  205 MiB     14 ms      3d 04h
neuroos-inference    ✔ OK       1,561 / 1,590 MiB    44 ms/tok  3d 04h
neuroos-knowledge    ✔ OK          61 /   75 MiB      3.8 ms    3d 04h
neuroos-kernel       ✔ OK          13 /   15 MiB      0.2 ms    3d 04h
neuroos-fetcher      ▲ DEGRADED     —  /   10 MiB     —         restarting (2)

hint: run `neuroosctl logs neuroos-fetcher` for details
```

### 8.3 Rules

- Every command supports `--json` (stable schema, versioned) and `--quiet`.
- Exit codes: `0` success, `1` failure, `2` usage error, `3` degraded, `4` denied by policy.
- Honour `NO_COLOR`, `--color=auto|always|never`, and non-TTY (no colour, no spinners).
- Tables align numbers right and use tabular spacing; width adapts to the terminal, minimum 80 columns.
- Destructive commands (`forget`, `purge`) always go through C6 (REVIEW dialog) and print what will be removed first.

---

## 9. Voice and Conversation Design (S-1)

### 9.1 Persona

Calm, competent colleague. Brief, concrete, never chatty, never overconfident. It says "I don't know" when evidence is missing, and it never pretends to have done something it did not do.

### 9.2 Voices

**Single voice rule:** NeuroOS has exactly one voice. Every spoken word (preambles, answers, errors, confirmations) comes from the same Kokoro-82M voice at the same speed. There is no second TTS engine.

| Engine | Role | Default voice (final pick in Phase 6 voice spike) | Rate |
| :--- | :--- | :--- | :--- |
| Kokoro-82M (24 kHz) | Everything: live answers + pre-rendered clips | `af_heart` (candidates to compare: `af_heart`, `af_bella`, `am_michael`, `bf_emma`) | 165–175 wpm |

**How preambles stay instant in the same voice:** at startup, Kokoro renders every line in §9.3 into memory-cached audio clips. Playing a cached clip takes ≤ 50 ms. Clips are re-rendered whenever the voice or speed setting changes.

**Sounding human in conversation:**
1. Speak in short sentences with contractions ("I've", "it's"). Never read lists, markdown or symbols.
2. Synthesize sentence by sentence and add natural pauses: ~250 ms at a full stop, ~120 ms at a comma.
3. Vary preambles and acknowledgements; never repeat the same phrase twice in a row.
4. Keep the conversation open: after answering, listen 8 s for a follow-up without the wake word.
5. Stop instantly when interrupted (barge-in), then answer the new question.
6. Keep one consistent persona and speed. Never change voice to signal errors.

### 9.3 Preamble catalogue (pre-synthesised, ≤ 1.2 s each)

| Intent | Phrases (rotate, never the same twice in a row) |
| :--- | :--- |
| Lookup / summarise | "Checking your files…", "Let me look.", "One moment." |
| Deictic ("this") | "Looking at that now…", "Checking what's on screen…" |
| Web research | "I'll need your OK for that." |
| Action (REVIEW) | "I'll ask you to confirm first." |
| Degraded | "I can't think right now — the model is restarting." |
| Not understood | "Sorry, I didn't catch that." |

### 9.4 Answer rules

1. Default spoken answer ≤ 60 words (≈ 20 s). Offer more: "Want the details?"
2. Lead with the answer, then one supporting fact. No preambles inside answers.
3. Never read raw URLs, hashes, stack traces or code aloud. Summarise ("the error is a missing lifetime in parser dot r s, line 104").
4. Mention when an answer uses web content: "According to the page you fetched…".
5. Confirmations are spoken as questions only when a dialog is on screen: "I've put a confirmation on screen."
6. Numbers: round for speech ("about two seconds"), exact in the CLI.

### 9.5 Earcons (optional, P1)

Two short tones, ≤ 150 ms, −18 LUFS: *listening* (rising two-note) and *cancelled* (single low note). No tone on success (DP-1).

---

## 10. Accessibility Checklist (every surface)

- [ ] Text contrast ≥ 4.5 : 1, large text and UI components ≥ 3 : 1 (tokens above are pre-checked).
- [ ] Meaning never by colour alone (text label + icon/shape always present).
- [ ] Full keyboard operation; visible 2 px focus ring.
- [ ] Screen-reader names for every control; dialog announces tier and taint first.
- [ ] Respects `prefers-reduced-motion` and `prefers-color-scheme`.
- [ ] Voice features have a text equivalent (`neuroosctl ask`, `audit tail`).
- [ ] Minimum target size 24 × 24 px for pointer controls.

---

## 11. Design Tokens File Format

`ui/tokens/tokens.json` (single source; build generates `tokens.css`, `gtk.css`, `ansi.rs`):

```json
{
  "$schema": "neuroos-tokens/v1",
  "color": {
    "dark":  { "bg.canvas": "#0E1116", "text.primary": "#E6EDF3", "accent.primary": "#2DD4BF",
               "tier.safe": "#3FB950", "tier.review": "#D29922", "tier.dangerous": "#F85149",
               "taint.external": "#DB61A2" },
    "light": { "bg.canvas": "#FFFFFF", "text.primary": "#1F2328", "accent.primary": "#0F766E",
               "tier.safe": "#1A7F37", "tier.review": "#9A6700", "tier.dangerous": "#CF222E",
               "taint.external": "#BF3989" }
  },
  "font": {
    "sans": "Inter, 'Open Sans', 'Noto Sans', system-ui, sans-serif",
    "mono": "'JetBrains Mono', 'Noto Sans Mono', 'DejaVu Sans Mono', ui-monospace, monospace"
  },
  "type": { "base": 14, "ratio": 1.2 },
  "space": [0, 4, 8, 12, 16, 20, 24, 32, 40, 48],
  "radius": { "sm": 4, "md": 8, "lg": 12 }
}
```

(The full file includes every token listed in §3–§5.)
