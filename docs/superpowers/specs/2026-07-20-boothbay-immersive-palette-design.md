# Boothbay-Inspired Immersive Desktop Palette Design

## Goal

Create a Tailwind CSS color theme and a high-fidelity Crosstache Vault desktop mockup based on the rendered brand palette of [bbaymgmt.com](https://www.bbaymgmt.com/). The result should preserve Crosstache's current information architecture while adopting an immersive, deep-blue visual treatment.

## Source Palette

The following colors were observed in the live page's rendered styles and brand sections:

| Token | Value | Intended role |
| --- | --- | --- |
| `boothbay-navy` | `#0E0F52` | Primary canvas and darkest branded surface |
| `boothbay-deep` | `#0F1974` | Elevated navigation and drawer surfaces |
| `boothbay-blue` | `#192AC2` | Selected controls and prominent branded panels |
| `boothbay-cyan` | `#08A2D1` | Primary actions, active states, and status indicators |
| `boothbay-sky` | `#A5D6FE` | Borders, subdued labels, and quiet highlights |
| `boothbay-white` | `#FFFFFF` | Primary text and high-contrast iconography |

Only these observed brand colors will be exposed as named Tailwind palette tokens. Semantic aliases may use the same values, but the implementation will not invent an artificial numbered color scale.

## Tailwind Artifact

Create `/Users/scottzionic/crosstache/tailwind.css` as a standalone Tailwind CSS v4 entrypoint. It will:

- import Tailwind CSS;
- expose the six exact source colors through `@theme` custom properties;
- provide semantic CSS custom properties for canvas, surface, elevated surface, primary action, border, primary text, and muted text;
- include no component styling or changes to the existing Crosstache application.

The semantic mapping is:

- canvas: `boothbay-navy`;
- surface: `boothbay-deep`;
- elevated surface and selected control: `boothbay-blue`;
- primary action and status: `boothbay-cyan`;
- border and muted text: `boothbay-sky`;
- primary text: `boothbay-white`.

## Interface Mockup

Generate `/Users/scottzionic/crosstache/docs/mockups/crosstache-boothbay-immersive.png` as a landscape, high-fidelity macOS Tauri interface concept.

The mockup will retain the established Crosstache Vault layout:

- native macOS window framing;
- `xv` mark and `Crosstache Vault` product name;
- backend badge, vault selector, and Secrets/Files tabs;
- secrets heading, search, selection action, and New secret action;
- six-row secret table with concealed values;
- open New secret drawer with the existing form hierarchy.

The visual treatment will use the navy canvas throughout, deep-blue surfaces for navigation and the drawer, royal blue for selected or raised areas, cyan for primary actions, pale blue for borders and secondary information, and white for primary text. Shadows may use translucent navy only. It will not copy Boothbay logos, photography, typography, or marketing language.

## Accessibility and Visual Constraints

- White remains the default text color on navy and royal-blue surfaces.
- Cyan is reserved for actions, focus, and live status so it retains meaning.
- Pale blue provides separation without introducing gray.
- Secret values remain concealed in every row and form control.
- The image must avoid gradients, glassmorphism, generic analytics charts, and unrelated colors.
- The concept is a visual artifact only; it does not alter the production desktop CSS.

## Verification

- Confirm `tailwind.css` contains all six exact source hex values and valid Tailwind v4 `@theme` syntax.
- Confirm the generated image exists at the specified project path and can be decoded as a PNG.
- Visually verify the mockup preserves the Crosstache layout, uses the extracted palette, keeps secret values concealed, and contains no Boothbay brand assets.

