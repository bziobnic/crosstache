# Boothbay-Inspired Immersive Palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a Tailwind CSS v4 theme containing Boothbay's extracted brand colors and a project-local Crosstache Vault desktop mockup that applies the approved immersive treatment.

**Architecture:** Keep the deliverables independent from production application code: `tailwind.css` is a standalone Tailwind v4 theme entrypoint, and the PNG is a visual concept under `docs/mockups`. The CSS exposes exact observed colors plus semantic aliases; the raster mockup reuses Crosstache's established layout without copying Boothbay brand assets.

**Tech Stack:** Tailwind CSS v4 CSS-first theme variables, CSS custom properties, built-in OpenAI image generation, macOS `file` and `sips` verification tools.

## Global Constraints

- Use exactly these extracted colors: `#0E0F52`, `#0F1974`, `#192AC2`, `#08A2D1`, `#A5D6FE`, and `#FFFFFF`.
- Do not invent a numbered palette scale.
- Do not modify the production Crosstache desktop CSS or application behavior.
- Do not reproduce Boothbay logos, photography, typography, or marketing language.
- Keep all depicted secret values concealed.
- Avoid gradients, glassmorphism, analytics charts, and unrelated colors.
- Save the CSS at `/Users/scottzionic/crosstache/tailwind.css`.
- Save the image at `/Users/scottzionic/crosstache/docs/mockups/crosstache-boothbay-immersive.png`.

---

### Task 1: Tailwind Theme Artifact

**Files:**
- Create: `tailwind.css`

**Interfaces:**
- Consumes: six exact source colors approved in `docs/superpowers/specs/2026-07-20-boothbay-immersive-palette-design.md`.
- Produces: Tailwind utilities such as `bg-boothbay-navy`, `text-boothbay-white`, and `border-boothbay-sky`, plus semantic CSS properties for non-utility consumers.

- [ ] **Step 1: Confirm the destination is still absent**

Run:

```bash
test ! -e tailwind.css
```

Expected: exit status `0`. If the file now exists, inspect it before proceeding and do not overwrite unrelated user content.

- [ ] **Step 2: Create the Tailwind v4 entrypoint**

Create `tailwind.css` with exactly:

```css
@import "tailwindcss";

@theme {
  --color-boothbay-navy: #0e0f52;
  --color-boothbay-deep: #0f1974;
  --color-boothbay-blue: #192ac2;
  --color-boothbay-cyan: #08a2d1;
  --color-boothbay-sky: #a5d6fe;
  --color-boothbay-white: #ffffff;
}

:root {
  --color-canvas: var(--color-boothbay-navy);
  --color-surface: var(--color-boothbay-deep);
  --color-surface-elevated: var(--color-boothbay-blue);
  --color-action-primary: var(--color-boothbay-cyan);
  --color-border: var(--color-boothbay-sky);
  --color-text: var(--color-boothbay-white);
  --color-text-muted: var(--color-boothbay-sky);
}
```

- [ ] **Step 3: Verify the theme tokens and semantic aliases**

Run:

```bash
rg -n --fixed-strings '@import "tailwindcss";' tailwind.css
rg -n -- '--color-boothbay-(navy|deep|blue|cyan|sky|white):' tailwind.css
rg -n -- '--color-(canvas|surface|surface-elevated|action-primary|border|text|text-muted):' tailwind.css
git diff --check -- tailwind.css
```

Expected: one import match, six Boothbay token matches, seven semantic alias matches, and no `git diff --check` output.

- [ ] **Step 4: Commit the CSS artifact**

Run:

```bash
git add -- tailwind.css
git commit -m "feat: add Boothbay-inspired Tailwind palette"
```

Expected: one new file committed and no unrelated files staged.

---

### Task 2: Immersive Crosstache Interface Mockup

**Files:**
- Create: `docs/mockups/crosstache-boothbay-immersive.png`

**Interfaces:**
- Consumes: the exact palette and semantic roles defined in `tailwind.css`; the current Crosstache structure in `src/web/assets/index.html`; the visual hierarchy in `src/web/assets/style.css`.
- Produces: a landscape PNG concept that can be reviewed independently without being loaded by production code.

- [ ] **Step 1: Mark the start of the generation run**

Run:

```bash
touch /tmp/crosstache-boothbay-imagegen-start
```

Expected: the marker exists immediately before image generation.

- [ ] **Step 2: Generate one high-fidelity interface image**

Use the `imagegen` skill and the built-in image generation tool with this exact prompt:

```text
Use case: ui-mockup
Asset type: high-fidelity visual concept for the Crosstache Vault macOS Tauri desktop application
Primary request: Create an immersive, deep-ocean-blue version of the established Crosstache Vault interface. This is a real, usable secrets-management desktop UI, not a marketing website.
Scene/backdrop: a native macOS application window shown straight-on, with understated title-bar controls recolored within the supplied palette and no environmental background.
Interface structure: top navigation with a compact square “xv” mark, exact product name “Crosstache Vault”, backend badge “LOCAL”, vault selector “Personal Vault”, online status, and segmented “Secrets” active / “Files” inactive tabs. Main content includes eyebrow “VAULT CONTENTS”, heading “Your secrets”, helper copy “Browse, organize, and safely manage credentials in this vault.”, count “6 secrets”, search placeholder “Search by name, folder, group, or note”, “Select” and primary “New secret” buttons, and a six-row secrets table. Table headers are “NAME”, “FOLDER”, “GROUPS”, “NOTE”, and “UPDATED”. Rows are “Stripe API Key”, “GitHub Token”, “AWS Production”, “Database Password”, “OpenAI API Key”, and “Sentry DSN”. Include an open right-side drawer titled “New secret” with fields Name, Value, Folder, Groups, Note, and Expires, plus Cancel and Save secret actions. Keep the Value field concealed.
Style/medium: crisp contemporary 2D desktop product UI, restrained native Tauri/macOS character, compact table density, precise typography, generous alignment, subtle depth, no illustration.
Composition/framing: full landscape 16:10 app window, orthographic view, interface filling the image, right drawer occupying about 30 percent of the width.
Color palette: use only Boothbay Navy #0E0F52, Boothbay Deep #0F1974, Boothbay Blue #192AC2, Boothbay Cyan #08A2D1, Boothbay Sky #A5D6FE, and White #FFFFFF. Use #0E0F52 for the canvas, #0F1974 for navigation and drawer surfaces, #192AC2 for selected controls and elevated panels, #08A2D1 for primary actions, focus, and live status, #A5D6FE for borders and secondary text, and #FFFFFF for primary text and icons. Shadows may use translucent #0E0F52 only.
Constraints: preserve accessible contrast; keep all secret values concealed; use no Boothbay logo, photography, typography, or marketing copy; no browser address bar; no people; no watermark.
Avoid: gradients, glassmorphism, generic analytics charts, unrelated hues, gray panels, oversized cards, illegible microtext, decorative locks, or skeuomorphic vault imagery.
```

Expected: one landscape PNG and an absolute generated-file path reported under `/Users/scottzionic/.codex/generated_images/`.

- [ ] **Step 3: Copy the newly generated image into the project**

Run:

```bash
generated_mockup_path=$(find /Users/scottzionic/.codex/generated_images -type f -name '*.png' -newer /tmp/crosstache-boothbay-imagegen-start -print | head -n 1)
test -n "$generated_mockup_path"
mkdir -p docs/mockups
cp -f "$generated_mockup_path" docs/mockups/crosstache-boothbay-immersive.png
rm -f /tmp/crosstache-boothbay-imagegen-start
```

Expected: the generated PNG is copied to the exact project path and the task-specific marker is removed.

- [ ] **Step 4: Verify the PNG mechanically and visually**

Run:

```bash
file docs/mockups/crosstache-boothbay-immersive.png
sips -g format -g pixelWidth -g pixelHeight docs/mockups/crosstache-boothbay-immersive.png
```

Expected: `file` identifies PNG image data; `sips` reports format `png`; pixel width is greater than pixel height.

Open the file with the image-viewing tool and confirm:

- the Crosstache header, table, and New secret drawer are present;
- all secret values remain concealed;
- the image is dominated by the six approved palette colors;
- no Boothbay logo, photograph, or marketing copy appears;
- no gradients, glassmorphism, analytics charts, or unrelated hues appear.

- [ ] **Step 5: Commit the mockup**

Run:

```bash
git add -- docs/mockups/crosstache-boothbay-immersive.png
git commit -m "docs: add immersive desktop palette mockup"
```

Expected: the PNG is committed with no unrelated files staged.

---

### Task 3: Final Verification and Handoff

**Files:**
- Verify: `tailwind.css`
- Verify: `docs/mockups/crosstache-boothbay-immersive.png`

**Interfaces:**
- Consumes: both completed artifacts.
- Produces: evidence that the theme and mockup satisfy the approved design without changing production code.

- [ ] **Step 1: Verify exact palette coverage and file integrity**

Run:

```bash
for color in 0e0f52 0f1974 192ac2 08a2d1 a5d6fe ffffff; do
  rg --fixed-strings --quiet "#$color" tailwind.css || exit 1
done
test "$(rg -o '#[0-9a-fA-F]{6}' tailwind.css | tr '[:upper:]' '[:lower:]' | sort -u | wc -l | tr -d ' ')" = "6"
file docs/mockups/crosstache-boothbay-immersive.png | rg --fixed-strings 'PNG image data'
git diff --check
```

Expected: exit status `0`, exactly six unique CSS hex values, PNG image data confirmed, and no whitespace errors.

- [ ] **Step 2: Confirm production code was not modified**

Run:

```bash
git status --short
git log -3 --oneline
```

Expected: the working tree is clean; the two artifact commits and this implementation-plan commit are the newest relevant commits; no production source file appears in either artifact commit.

- [ ] **Step 3: Synchronize the branch and report the artifacts**

Run:

```bash
git pull --rebase origin main
git push -u origin codex/boothbay-immersive-palette
```

Expected: the branch rebases onto current `origin/main` without conflicts before `codex/boothbay-immersive-palette` is pushed with its upstream remote configured.

Report clickable paths for `tailwind.css`, the PNG mockup, and the design/implementation documents, plus the verification results.
