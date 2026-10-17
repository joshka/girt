// Checks every external link in the built site, including docs.rs item anchors, which change when
// the API is renamed. Run after `pnpm build`: `pnpm check:links`.
import { access, readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';

const root = new URL('../dist/', import.meta.url).pathname;
const repository = new URL('../../', import.meta.url).pathname;
// Links to files in this repository are checked against the checkout, which is faster, avoids
// GitHub rate limits and also covers files added by the same change.
const ownFiles = /^https:\/\/github\.com\/joshka\/girt\/(?:blob|tree)\/main\/([^#?]+)/;
const ownSite = 'https://www.joshka.net/girt';
// "Edit page" links name files that exist on main only after merge, and GitHub rate-limits them.
const editLinks = 'https://github.com/joshka/girt/edit/';

async function* htmlFiles(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) yield* htmlFiles(path);
    else if (entry.name.endsWith('.html')) yield path;
  }
}

const decode = (url) => url.replaceAll('&amp;', '&');
const links = new Map();
for await (const file of htmlFiles(root)) {
  const html = await readFile(file, 'utf8');
  for (const [, url] of html.matchAll(/href="(https?:\/\/[^"]+)"/g)) {
    const link = decode(url);
    if (link.startsWith(ownSite) || link.startsWith(editLinks)) continue;
    links.set(link, [...(links.get(link) ?? []), file.slice(root.length)]);
  }
}

// crates.io pages need a browser, so check the crate through its API instead.
const fetchTarget = (base) => {
  const crate = base.match(/^https:\/\/crates\.io\/crates\/([^/]+)$/);
  return crate ? `https://crates.io/api/v1/crates/${crate[1]}` : base;
};

// A few requests at a time, retrying rate limits and transient server errors with backoff.
const MAX_IN_FLIGHT = 4;
let inFlight = 0;
const waiting = [];
async function limited(task) {
  if (inFlight >= MAX_IN_FLIGHT) await new Promise((resolve) => waiting.push(resolve));
  inFlight++;
  try {
    return await task();
  } finally {
    inFlight--;
    waiting.shift()?.();
  }
}

async function request(url) {
  for (let attempt = 0; ; attempt++) {
    const result = await limited(() =>
      fetch(url, {
        headers: { 'user-agent': 'girt-site-link-check (https://github.com/joshka/girt)' },
        signal: AbortSignal.timeout(30_000),
      }).then(
        async (response) => ({ status: response.status, body: await response.text() }),
        (error) => ({ status: String(error), body: '' }),
      ),
    );
    const transient =
      result.status === 429 || (typeof result.status === 'number' && result.status >= 500);
    if (!transient || attempt === 3) return result;
    await new Promise((resolve) => setTimeout(resolve, 2_000 * 2 ** attempt));
  }
}

const pages = new Map();
const load = (base) => {
  if (!pages.has(base)) {
    const file = base.match(ownFiles);
    pages.set(
      base,
      file
        ? access(join(repository, decodeURIComponent(file[1]))).then(
            () => ({ status: 200, body: '' }),
            () => ({ status: 'missing in checkout', body: '' }),
          )
        : request(fetchTarget(base)),
    );
  }
  return pages.get(base);
};

const failures = [];
await Promise.all(
  [...links.keys()].map(async (link) => {
    const [base, fragment] = link.split('#');
    const { status, body } = await load(base);
    if (status !== 200) failures.push(`${status} ${link}`);
    else if (fragment && base.startsWith('https://docs.rs/') && !body.includes(`id="${fragment}"`)) {
      failures.push(`missing anchor ${link}`);
    }
  }),
);

for (const failure of failures.sort()) {
  const url = failure.slice(failure.indexOf('http'));
  console.error(`${failure}\n  in ${links.get(url).join(', ')}`);
}
console.log(`${links.size} external links checked, ${failures.length} broken`);
process.exit(failures.length ? 1 : 0);
