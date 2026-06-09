// The rekody version, injected at build time from the workspace Cargo.toml
// via Vite `define` in astro.config.mjs. (Reading the file here at module
// eval broke on Astro 6 — prerender chunks moved, so relative paths to the
// repo root no longer resolved.)
declare const __REKODY_VERSION__: string;

export const version = __REKODY_VERSION__;
