# Rekody use-cases page — spot illustration prompts

One consistent system: single-weight ink line art + flat teal fills on pure white.
Rationale: white ground is native to line art (the page is the paper), it drifts the
least across 9+ generations, and it matches the site's hairline/serif editorial feel.
Set rules: ink + teal ONLY (no moss/amber in this set); recurring five-bar teal
soundwave motif in every image; people faceless or from behind; zero text anywhere.

## STYLE PREAMBLE (paste at the front of every prompt)

Minimal editorial spot illustration in single-weight ink line art on a pure white #FFFFFF background. Confident smooth contour lines in near-black ink (#0F1717), one consistent line weight throughout, generous empty white margins so the drawing floats on the page — no border, no frame, no background wash, no scenery beyond the objects described. The only color is flat solid teal (#20808D), used sparingly as fill accents on two or three objects, with pale teal (#7FD4DE) allowed for one small secondary fill; everything else stays white paper or ink line. Recurring motif: a small floating soundwave made of five rounded vertical teal bars of varying heights. Any people are faceless or seen from behind, drawn simply. Absolutely no text, letters, numbers, words, or logos anywhere in the image. Calm, premium, hand-drawn editorial style, like a New Yorker or Monocle magazine spot illustration.

## Per-persona prompts (append after the preamble)

### 1 — Developers & engineers
A developer's desk from a three-quarter angle: an open laptop whose screen shows only an abstract branching diagram of small dots connected by curved lines, a mechanical keyboard deliberately pushed off to the side, a coffee mug, and one relaxed hand resting on the desk. The teal five-bar soundwave floats just above the laptop screen. Flat teal fill on the mug and on three dots of the branching diagram.

### 2 — Writers & journalists
Overhead flat-lay of a journalist's workspace: an open reporter's spiral notebook filled with loose wavy scribble lines that are clearly squiggles and not letters, an uncapped fountain pen laid diagonally across it, a small handheld voice recorder, and one crumpled ball of paper. The teal five-bar soundwave rises from the voice recorder. Flat teal fill on the pen cap and the recorder's round button.

### 3 — Students
A student seen from behind, sitting at a small lecture-hall desk: a stack of three textbooks beside an open notebook of squiggle lines, and a simple hexagonal molecule diagram of linked rings floating above their head like a thought. The teal five-bar soundwave hovers in the air beside their shoulder. Flat teal fill on one textbook cover and on two nodes of the molecule diagram.

### 4 — Founders & operators
Full-body side profile of a founder mid-stride on a single thin ground line, faceless, one hand gesturing as they talk, a tiny wireless earbud dot at the ear. The teal five-bar soundwave floats in the air just ahead of their face. Behind them floats a minimal ascending stair-step line, an abstract growth curve with no frame and no labels. Flat teal fill on the jacket and a pale teal wash under the ascending curve.

### 5 — Designers & PMs
Close-up, slightly top-down view of two hands over a drawing tablet: one hand holds a stylus hovering above the surface, the other rests at the tablet's edge. On the tablet, blank wireframe rectangles and one circle-and-line placeholder card, all empty. Above the wireframe floats a rounded speech-bubble outline containing the teal five-bar soundwave. Flat teal fill on one wireframe rectangle and the stylus grip.

### 6 — Researchers & academics
Straight-on still life of an academic's desk: a tall stack of loose manuscript pages drawn as blank sheets with faint squiggle lines, folded reading glasses resting on top, and a hand entering from the right edge of the frame holding one page up. Above the stack floats a simple line chart with a single plotted curve and small error bars, no axis marks. The teal five-bar soundwave sits beside the held page. Flat teal fill under the chart curve and on the mug at the far left.

### 7 — Lawyers
Side profile of a lawyer mid-stride on a thin ground line, faceless, a briefcase in one hand and a thick bound folio tucked under the other arm. Behind them, one tall classical column drawn as two simple vertical lines with a plain capital, hinting at a courthouse hallway. The teal five-bar soundwave floats just ahead of their face. Flat teal fill on the briefcase and a teal band on the folio's spine.

### 8 — Healthcare
Close-up still life on a clinician's desk: a stethoscope coiled in a loose loop, a clipboard whose chart is only blank rows of squiggle lines with one small heartbeat spike line, and a hand resting beside the clipboard holding a pen at ease, not writing. The teal five-bar soundwave rises from beside the stethoscope's chest piece. Flat teal fill on the stethoscope tubing and on the heartbeat spike line.

### 9 — Everyday
A cozy kitchen-counter morning vignette from a three-quarter angle: a hand wrapped around a steaming mug, a smartphone with a completely blank screen propped against a small potted plant, a set of house keys, and a small notepad showing only a column of empty checkbox squares. The teal five-bar soundwave floats between the mug and the phone. Flat teal fill on the mug, the plant pot, and two of the checkbox squares.

### 10 — OPTIONAL: "Hands-free by design" accessibility panel
Macro close-up of a single oversized blank keycap sitting on a desk, with a forearm wearing a soft fabric wrist brace resting gently beside it, one fingertip touching the key without pressing it. The teal five-bar soundwave rises from just above the keycap. Flat teal fill on the keycap's top face and a thin teal band on the wrist brace.

NOTE: on the live page this section sits on the dark ink (#0F1717) panel, not white.
If embedding there, swap the preamble's first sentence to: "…white and pale-teal line
art on a solid near-black #0F1717 background…". Otherwise use only on a white area.

## Shared NEGATIVE prompt / avoid-list

text, letters, words, numbers, typography, handwriting, labels, captions, logo, watermark, signature, readable screen, user interface, app window, keyboard key legends, cream background, beige background, ivory, off-white, sepia, warm paper texture, background gradient, colored background, red, orange, purple, pink, yellow, green, brown, multiple accent colors, 3D render, CGI, plastic sheen, heavy drop shadows, corporate memphis, blob characters, tiny heads, stock photo, photorealistic, realistic skin, detailed face, grain, noise, halftone, cross-hatching, frame, border

## Generator settings

- Aspect/size: 1:1 at 1024×1024 for the whole set (crisp at the 600–800px embed size).
  If you prefer wide card headers, 4:3 — but pick ONE ratio for all 9, never mix.
- Count: 4 variants per persona, pick the best.
- Recraft (recommended): Recraft V3, style family "Vector Illustration", substyle
  "Line Art" ("Thin" fallback if fills come out heavy). Avoid-list goes in the
  Negative Prompt field. CONSISTENCY LOCK: perfect persona 1 first, then create a
  custom Style from the 2–3 best outputs and generate the other 8 under that locked
  style. Export SVG — recolor teal to exact #20808D in-file if it drifts.
- Midjourney: append --ar 1:1 --style raw --no text, letters, logos, cream, beige,
  gradient, 3d, photo. After the first approved image, add --sref <its URL> --sw 200.
- gpt-image: no negative field — end the prompt with "Do not include:" + avoid-list,
  and restate "the background must be pure #FFFFFF white" as the final sentence.
- QA before embedding: sample each output's background pixel — generators often emit
  ~rgb(250,250,250); a white-point bump makes them melt into the page. Confirm teal
  lands near #20808D across the set.
