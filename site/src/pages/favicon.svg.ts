// Starlight links /favicon.svg from every page. Generate it at build time so the repository commits
// no image files: a teal tile with three commits and a merge, drawn in the site's accent colour.
import type { APIRoute } from 'astro';

const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect width="32" height="32" rx="7" fill="#128275"/>
  <g fill="none" stroke="#f2fffc" stroke-width="2.5" stroke-linecap="round">
    <path d="M11 8v16"/>
    <path d="M21 12c0 6-10 5-10 10"/>
  </g>
  <g fill="#f2fffc">
    <circle cx="11" cy="8" r="3"/>
    <circle cx="21" cy="11" r="3"/>
    <circle cx="11" cy="24" r="3"/>
  </g>
</svg>
`;

export const GET: APIRoute = () =>
  new Response(svg, { headers: { 'Content-Type': 'image/svg+xml' } });
