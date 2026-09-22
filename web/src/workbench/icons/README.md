# Codicons subset

The 20 glyphs in `codicons.ts` come from [Microsoft Codicons](https://github.com/microsoft/vscode-codicons), pinned to commit [`6833ea2bc5fc49220e261a4a0fd1986ea02d1d0c`](https://github.com/microsoft/vscode-codicons/tree/6833ea2bc5fc49220e261a4a0fd1986ea02d1d0c/src/icons), retrieved on 2026-09-21.

Author: Microsoft Corporation. License: [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/); the full upstream license is included in [LICENSE](LICENSE), with only line endings and trailing whitespace normalized. The license wording is unchanged. The material is provided without warranty under that license. This project is not affiliated with or endorsed by Microsoft.

Adaptation: each SVG's `viewBox`, path `d`, `fill-rule` and `clip-rule` attributes have been extracted into a static TypeScript object. Missing fill/clip rules use the SVG default `nonzero`; geometry is unchanged. The application controls display color and size via CSS. SVG wrappers are rendered with Preact, without HTML injection, an icon font, remote asset requests or a runtime package dependency.

Object keys correspond exactly to the upstream `src/icons/<key>.svg` filenames. `Icons.tsx` maps the workbench's semantic names to this subset. Preserve the source and license comment when updating or distributing these icons.
