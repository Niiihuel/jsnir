# Reference: the original React Bits Stepper

The original component is the React Bits Stepper (MIT + Commons Clause, © 2026 David Haz), taken from
[DavidHDev/react-bits at fefc5b0](https://github.com/DavidHDev/react-bits/tree/fefc5b05ca34a7475b38a977127441fbfad2f177/src/content/Components/Stepper).
Its license does not allow redistributing the component itself, so it is not in this repository:
`build.sh` downloads it, unchanged, into `original/`.
`app.jsx` is the "onboarding" demo from React Bits' `StepperDemo.jsx`, reading its state from URL params
(`step` 1–5, `name`, `plan`, `focus=1`, `capture`). `dist/` is the bundle `build.sh` writes; after building, viewing needs no network.

```sh
./build.sh     # download original/ and build dist/ (needs curl, npm and network; versions pinned)
./capture.sh   # write ../captures/react-s1.png … react-s6.png with headless Chromium
```

Open `index.html?step=2` in a browser to see a state with animations. With `capture`, the caret is hidden
and transitions and Web Animations jump to their end, so the captures show settled states and are
reproducible run to run.
