// @ts-check
import { defineConfig } from 'astro/config';
import { readFileSync } from 'node:fs';
import starlight from '@astrojs/starlight';
import { ExpressiveCodeTheme } from '@astrojs/starlight/expressive-code';
import starlightLinksValidator from 'starlight-links-validator';

const base = '/girt';

// Code themes from DESIGN.md: muted rust keywords, verdigris strings, nothing bright.
/** @param {string} name */
const codeTheme = (name) =>
  ExpressiveCodeTheme.fromJSONString(readFileSync(new URL(`./src/styles/${name}.json`, import.meta.url), 'utf8'));

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
      logo: {
        light: './src/assets/logo-light.svg',
        dark: './src/assets/logo-dark.svg',
        replacesTitle: true,
      },
      customCss: ['./src/styles/custom.css'],
      components: {
        Head: './src/components/Head.astro',
        Hero: './src/components/Hero.astro',
        Sidebar: './src/components/Sidebar.astro',
        SocialIcons: './src/components/SocialIcons.astro',
        ThemeProvider: './src/components/ThemeProvider.astro',
      },
      expressiveCode: {
        themes: [codeTheme('girt-dark'), codeTheme('girt-light')],
        useStarlightUiThemeColors: false,
        // Shell snippets get the same plain inset panel as other code, not a terminal window.
        defaultProps: { frame: 'code' },
        styleOverrides: {
          borderRadius: '8px',
          borderColor: 'var(--sl-color-hairline-light)',
          codeFontFamily: 'var(--__sl-font-mono)',
          codeFontSize: '0.84375rem',
          codeLineHeight: '1.7',
          codePaddingBlock: '0.9rem',
          codePaddingInline: '1.1rem',
          uiFontFamily: 'var(--__sl-font-mono)',
          frames: {
            frameBoxShadowCssValue: ({ theme }) =>
              theme.type === 'dark' ? 'inset 0 2px 10px rgb(0 0 0 / 0.3)' : 'inset 0 1px 3px rgb(0 0 0 / 0.06)',
            editorActiveTabBackground: 'transparent',
            editorActiveTabIndicatorTopColor: 'transparent',
            editorActiveTabIndicatorBottomColor: 'transparent',
            editorTabBarBackground: 'transparent',
            editorTabBarBorderBottomColor: 'var(--sl-color-hairline-light)',
            editorActiveTabForeground: 'var(--sl-color-gray-3)',
            terminalTitlebarBackground: 'transparent',
            terminalTitlebarBorderBottomColor: 'var(--sl-color-hairline-light)',
            terminalTitlebarForeground: 'var(--sl-color-gray-3)',
            terminalBackground: 'var(--girt-inset)',
            tooltipSuccessBackground: 'var(--girt-verdigris-700)',
          },
        },
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
          label: 'Command-line tool',
          items: [
            { label: 'Overview', slug: 'cli' },
            { label: 'Getting started', slug: 'cli/getting-started' },
            { label: 'Repositories and objects', slug: 'cli/objects' },
            { label: 'References', slug: 'cli/references' },
            { label: 'History and blame', slug: 'cli/history' },
            { label: 'Diffs', slug: 'cli/diffs' },
            { label: 'Index and working tree', slug: 'cli/index-and-working-tree' },
            { label: 'Configuration and remotes', slug: 'cli/configuration-and-remotes' },
            { label: 'Fetch, push and clone', slug: 'cli/fetch-push-clone' },
            { label: 'Packs and maintenance', slug: 'cli/storage-and-maintenance' },
            { label: 'Comparing with Git', slug: 'cli/comparing-with-git' },
          ],
        },
        {
          label: 'How Git works',
          items: [
            { label: 'Overview', slug: 'how-git-works' },
            { label: 'Storage', slug: 'how-git-works/storage' },
            { label: 'References', slug: 'how-git-works/references' },
            { label: 'Configuration and revisions', slug: 'how-git-works/config-and-revisions' },
            { label: 'The working tree', slug: 'how-git-works/working-tree' },
            { label: 'Diffs and blame', slug: 'how-git-works/diff' },
            { label: 'Tags, worktrees and more', slug: 'how-git-works/other' },
            { label: 'Transfer', slug: 'how-git-works/transfer' },
            { label: 'Combining and exchanging work', slug: 'how-git-works/combining-work' },
            { label: 'Scale, serving and tooling', slug: 'how-git-works/scale-and-serving' },
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
            { label: 'Limit operations to paths', slug: 'guides/pathspecs' },
            { label: 'Read and update refs', slug: 'guides/refs' },
            { label: 'Configuration and provenance', slug: 'guides/configuration' },
            { label: 'The index and working tree', slug: 'guides/index-and-working-tree' },
            { label: 'Line endings and filters', slug: 'guides/filters' },
            { label: 'Sign and verify commits', slug: 'guides/signatures' },
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
