// Deterministic identity colors. Providers get fixed categorical slots
// (validated palette, see web/README.md); anything else hashes into the same
// eight slots so a provider always gets the same color everywhere.

const KNOWN: Record<string, number> = {
  anthropic: 2, // orange
  openai: 3, // aqua
  google: 1, // blue
  xai: 7, // violet
  mistral: 4, // yellow
  meta: 5, // magenta
  deepseek: 6, // green
};

/** FNV-1a, stable across sessions and browsers. */
export function hash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Categorical slot 1..8 for a provider. */
export function providerSlot(provider: string): number {
  const p = provider.toLowerCase();
  return KNOWN[p] ?? (hash(p) % 8) + 1;
}

/** CSS color for a provider (a token, so it follows the theme). */
export function providerColor(provider: string): string {
  return `var(--cat-${providerSlot(provider)})`;
}
