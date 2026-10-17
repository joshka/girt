// Starlight links /favicon.svg from every page. Build it from the logo so the favicon always
// matches: the cog and its commit graph, cropped to a square without the wordmark. It uses the
// light logo's colours (rust cog, ivory graph) and switches to the dark logo's colours (ivory cog,
// rust graph) when the browser prefers a dark scheme, so the cog stays visible on dark tab bars.
import type { APIRoute } from 'astro';
import darkLogo from '../assets/logo-dark.svg?raw';
import lightLogo from '../assets/logo-light.svg?raw';

// The cog's teeth reach 273 units from its centre at (284, 279) in the logo's coordinates.
const viewBox = '11 6 546 546';

function path(logo: string, part: 'cog' | 'graph'): string {
  const element = logo.match(new RegExp(`<path id="[^"]*-${part}"[^>]*/>`))?.[0];
  if (!element) {
    throw new Error(`a logo in src/assets/ has no <path id="…-${part}"> for the favicon`);
  }
  return element;
}

function fill(element: string): string {
  const colour = element.match(/ fill="(#[0-9a-f]{6})"/i)?.[1];
  if (!colour) {
    throw new Error(`a logo path in src/assets/ has no hex fill for the favicon: ${element.slice(0, 60)}`);
  }
  return colour;
}

// Drop the logo's ids so the CSS below targets stable class names instead.
const withClass = (element: string, name: string) => element.replace(/ id="[^"]*"/, ` class="${name}"`);

const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}">
  <style>
    @media (prefers-color-scheme: dark) {
      .cog { fill: ${fill(path(darkLogo, 'cog'))}; }
      .graph { fill: ${fill(path(darkLogo, 'graph'))}; }
    }
  </style>
  ${withClass(path(lightLogo, 'cog'), 'cog')}
  ${withClass(path(lightLogo, 'graph'), 'graph')}
</svg>
`;

export const GET: APIRoute = () =>
  new Response(svg, { headers: { 'Content-Type': 'image/svg+xml' } });
