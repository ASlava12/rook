import { el } from './lib.js';

const paths = {
  rook: 'M5 3v5l2 2v7l-2 2v2h14v-2l-2-2v-7l2-2V3h-4v3h-2V3h-2v3H9V3Z M7 17h10 M7 10h10',
  plus: 'M12 5v14M5 12h14', search: 'm21 21-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0',
  chat: 'M21 11a8 8 0 0 1-8 8H5l-4 3V11a10 10 0 0 1 20 0Z',
  task: 'm4 12 5 5L20 6', folder: 'M3 5h6l2 2h10v13H3Z',
  branch: 'M6 6v12M6 8c0 6 12 0 12 6v4M8 4a2 2 0 1 1-4 0 2 2 0 0 1 4 0M8 20a2 2 0 1 1-4 0 2 2 0 0 1 4 0M20 20a2 2 0 1 1-4 0 2 2 0 0 1 4 0',
  panel: 'M3 4h18v16H3ZM9 4v16', changes: 'M14 2H5v20h14V7ZM14 2v6h5M8 12h8M8 16h8',
  clock: 'M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0M12 6v6l4 2',
  command: 'M9 7H6a3 3 0 1 1 3-3v16a3 3 0 1 1-3-3h12a3 3 0 1 1-3 3V4a3 3 0 1 1 3 3ZM9 7h6v10H9Z',
  sun: 'M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1 1M18 18l1 1M5 19l1-1M18 6l1-1',
  moon: 'M20 14A9 9 0 0 1 10 3a9 9 0 1 0 10 11Z',
  chevron: 'm9 5 7 7-7 7', close: 'm6 6 12 12M6 18 18 6',
  arrow: 'M12 19V5m-6 6 6-6 6 6', down: 'M12 5v14m-6-6 6 6 6-6',
  paperclip: 'm8 13 7-7a3 3 0 0 1 4 4L9 20a5 5 0 0 1-7-7L13 2',
  refresh: 'M20 7v5h-5M4 17v-5h5M19 11a7 7 0 0 0-12-5M5 13a7 7 0 0 0 12 5',
  code: 'm8 6-6 6 6 6m8-12 6 6-6 6m-3-16-2 20',
  settings: 'M4 6h16M4 12h16M4 18h16M8 3v6M16 9v6M10 15v6',
  link: 'm10 14 4-4M8 16l-1 1a4 4 0 0 1-6-6l5-5a4 4 0 0 1 6 0M16 8l1-1a4 4 0 0 1 6 6l-5 5a4 4 0 0 1-6 0',
  agents: 'M9 8a3 3 0 1 1-6 0 3 3 0 0 1 6 0M21 8a3 3 0 1 1-6 0 3 3 0 0 1 6 0M1 21v-3a5 5 0 0 1 10 0v3M13 21v-3a5 5 0 0 1 10 0v3',
};

export function icon(name) {
  if (!document.createElementNS) return el('span', { 'aria-hidden': 'true' });
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  for (const [key, value] of Object.entries({ viewBox: '0 0 24 24', width: 18, height: 18, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.6, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true', class: 'icon' })) node.setAttribute(key, value);
  const shape = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  shape.setAttribute('d', paths[name] || paths.code); node.append(shape);
  return node;
}
