import { defineConfig } from 'astro/config';
import tailwindcss from '@tailwindcss/vite';
import sitemap from '@astrojs/sitemap';
import react from '@astrojs/react';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// Read the rekody version from the workspace Cargo.toml at CONFIG time and
// inject it as a build constant. Reading it at module-eval time inside
// src/lib/version.ts broke on Astro 6: prerender chunks moved
// (dist/.prerender/) so relative paths to the repo root no longer resolve.
const cargoToml = readFileSync(
  fileURLToPath(new URL('../Cargo.toml', import.meta.url)),
  'utf8',
);
const versionMatch = cargoToml.match(/^\s*version\s*=\s*"([^"]+)"/m);
if (!versionMatch) throw new Error('Could not find version in Cargo.toml');

export default defineConfig({
  site: 'https://rekody.com',
  output: 'static',
  integrations: [sitemap(), react()],
  vite: {
    plugins: [tailwindcss()],
    define: {
      __REKODY_VERSION__: JSON.stringify(versionMatch[1]),
    },
  },
});
