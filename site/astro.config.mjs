// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

const base = '/girt';

// GitHub Pages serves `/girt` without a trailing slash; send it to the canonical index.
const canonicalBasePathScript = [
  `if (location.pathname === '${base}') {`,
  `  location.replace('${base}/' + location.search + location.hash);`,
  '}',
].join('\n');

export default defineConfig({
  site: 'https://www.joshka.net',
  base,
  vite: {
    // The performance page imports benchmark JSON from the repository's docs/ directory.
    server: { fs: { allow: ['..'] } },
  },
  integrations: [
    starlight({
      title: 'girt',
      description:
        'An idiomatic Rust Git library: objects, references, the index and transfer, with Git formats preserved exactly.',
      customCss: ['./src/styles/custom.css'],
      components: {
        ThemeProvider: './src/components/ThemeProvider.astro',
      },
      head: [{ tag: 'script', content: canonicalBasePathScript }],
      editLink: {
        baseUrl: 'https://github.com/joshka/girt/edit/main/site/',
      },
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/joshka/girt' },
      ],
      plugins: [
        starlightLinksValidator({
          errorOnInvalidHashes: true,
          errorOnLocalLinks: true,
        }),
      ],
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Overview', slug: 'overview' },
            { label: 'Getting started', slug: 'getting-started' },
          ],
        },
        {
          label: 'Concepts',
          items: [
            { label: 'Design principles', slug: 'concepts/design-principles' },
            { label: 'The Git model', slug: 'concepts/git-model' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Open a repository', slug: 'guides/open-a-repository' },
            { label: 'Read objects and history', slug: 'guides/objects-and-history' },
            { label: 'Diffs and renames', slug: 'guides/diffs-and-renames' },
            { label: 'Read and update refs', slug: 'guides/refs' },
            { label: 'Configuration and provenance', slug: 'guides/configuration' },
            { label: 'The index and working tree', slug: 'guides/index-and-working-tree' },
            { label: 'Fetch, push and clone', slug: 'guides/fetch-push-clone' },
            { label: 'Credentials and transports', slug: 'guides/credentials-and-transports' },
            { label: 'Limits and trust', slug: 'guides/limits-and-trust' },
            { label: 'Maintenance and GC', slug: 'guides/maintenance' },
          ],
        },
        {
          label: 'Compatibility and safety',
          items: [
            { label: 'Supported and refused', slug: 'compatibility/supported-and-refused' },
            { label: 'Safety model', slug: 'compatibility/safety-model' },
          ],
        },
        {
          label: 'jj',
          items: [{ label: 'girt as jj’s Git backend', slug: 'jj/backend' }],
        },
        {
          label: 'Performance',
          items: [{ label: 'jj on girt benchmark', slug: 'performance/jj-benchmark' }],
        },
        {
          label: 'Reference',
          items: [
            { label: 'API docs (docs.rs)', link: 'https://docs.rs/girt/latest/girt/' },
            { label: 'Crate (crates.io)', link: 'https://crates.io/crates/girt' },
            { label: 'Changelog', link: 'https://github.com/joshka/girt/blob/main/CHANGELOG.md' },
          ],
        },
      ],
    }),
  ],
});
