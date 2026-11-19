# Stepper comparison: shared spec

The React Bits Stepper in Chromium is the reference for the GPUI and JSNIR ports. This spec fixes the
scene, input states and animation timings for the three implementations. [Runs, captures and results](README.md)

## Source

- React Bits `Stepper` (MIT + Commons Clause, © 2026 David Haz), from
  <https://github.com/DavidHDev/react-bits/tree/fefc5b05ca34a7475b38a977127441fbfad2f177/src/content/Components/Stepper>
  (`react/build.sh` downloads it into `react/original/`, which is not committed). The demo content comes
  from <https://github.com/DavidHDev/react-bits/blob/fefc5b05ca34a7475b38a977127441fbfad2f177/src/demo/Components/StepperDemo.jsx>, "onboarding" preset.
- Icons: Hugeicons free (MIT), 24×24 viewBox, round caps and joins:
  - ArrowLeft01: `M15 6C15 6 9.00001 10.4189 9 12C8.99999 13.5812 15 18 15 18`
  - ArrowRight01: `M9.00005 6C9.00005 6 15 10.4189 15 12C15 13.5812 9 18 9 18`
  - Tick02: `M5 14L8.5 17.5L19 6.5`
  - CheckmarkCircle02: circle r=10 at (12,12) + `M8 12.5L10.5 15L16 9`
- Use Inter 4.1 Regular (400) and SemiBold (600) for all text. [Bundled fonts](../../assets/fonts)

## Scene

- Window client area 640 × 560 logical px; captures at scale 1.
- Page background `#0b0b0f`.
- Stepper props: theme `dark`, indicator `numbers`, orientation `horizontal`, transition `slide`, width 440,
  radius 24, frame on, linear off, footer on, texts Back / Continue / Complete. No accent color.
- Stepper position: left 100, top 48 (horizontally centered, top-aligned so height changes do not move it).
- Backdrop blur of the frame is a no-op over a solid background: omit it.
- Steps (onboarding):
  1. "Welcome" — h3 "Welcome aboard"; p "Set up your workspace in a few quick steps. It only takes a minute."
  2. "Plan" — h3 "Choose a plan"; p "You can change this at any time."; 2-column grid (gap 8, margin-top 14) of
     Options "Starter / Free forever" and "Pro / $12 per month"; Pro selected by default.
  3. "Profile" — h3 "What should we call you?"; text field, placeholder "Your name". Continue is disabled
     (opacity 0.4) while the trimmed name is empty.
  4. "Review" — h3 "Looking good, {name}" (", {name}" only if non-empty); p "You picked the {Pro|Starter} plan.
     Complete setup to open your workspace."
  - Completed content: Done "Workspace ready" / "Welcome in, {name}." (", {name}" only if non-empty).
- Styles: exactly those of the original `Stepper.css`, the dark theme variables in `Stepper.jsx`, and the
  `heading`, `copy`, `Option`, `Field` and `Done` styles of the demo.

## States to capture (after animations settle, ≥ 1 s)

| Id | State |
| --- | --- |
| s1 | Step 1, initial |
| s2 | Step 2, Pro selected |
| s3 | Step 3, empty field, not focused, Continue disabled |
| s4 | Step 3, name "Ada" typed, field focused (ring), caret hidden for the capture |
| s5 | Step 4 with name "Ada" and plan Pro |
| s6 | Completed with name "Ada" |

Set each state directly with the native command-line flags or Chromium query parameters `step`,
`name`, `plan` and `focus`. Use `step=5` for the completed state. Save PNGs at 640 × 560 in
`captures/` with the `react-`, `jsnir-` or `gpui-` prefix and the state ID.

## Animations (must match the original timings and curves)

- SPRING: the `linear(...)` easing in `Stepper.jsx` (29 samples, evenly spaced).
- Glider: transform/width 560 ms SPRING; opacity 200 ms ease.
- Pane enter: translateX(direction × 36px → 0) 560 ms SPRING; opacity keyframes 0 → 0 (at 25 %) → 1, 420 ms ease-out.
- Pane leave: opacity 1 → 0 140 ms cubic-bezier(0.2, 0, 0.4, 1); translateX(0 → −direction × 36px) 200 ms
  cubic-bezier(0.4, 0, 1, 1); removed when done. Viewport height 560 ms SPRING.
- Footer hide on completion: height → 0 480 ms SPRING, opacity 200 ms ease.
- Badge colors 150 ms ease; title color 200 ms ease; Option background/ring 200 ms ease; field ring 200 ms ease.
- Check mark stroke draw 360 ms cubic-bezier(0.22, 1, 0.36, 1).
- Buttons: hover brightness (back 1.12, next 0.9) 200 ms; press scale 0.96 + brightness 0.85 instantly.
