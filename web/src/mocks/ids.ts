// Deterministic, valid-looking prefixed ULIDs for fixtures.

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

function fnv(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

function randomPart(seed: string, len: number): string {
  let out = "";
  let h = fnv(seed);
  while (out.length < len) {
    h = Math.imul(h ^ (h >>> 15), 0x2c1b3c6d) >>> 0;
    h = (h ^ fnv(`${seed}:${out.length}`)) >>> 0;
    out += CROCKFORD[h % 32];
  }
  return out;
}

function encodeTime(ms: number): string {
  let out = "";
  let t = Math.max(0, Math.floor(ms));
  for (let i = 0; i < 10; i++) {
    out = CROCKFORD[t % 32] + out;
    t = Math.floor(t / 32);
  }
  return out;
}

/** Stable id for a fixture entity: same seed, same id, across reloads. */
export function stableId(prefix: string, seed: string): string {
  return `${prefix}_01K6${randomPart(`${prefix}:${seed}`, 22)}`;
}

/** Time-ordered id (for events), so sorting by id sorts by time. */
export function idAt(prefix: string, ms: number, seed: string): string {
  return `${prefix}_${encodeTime(ms)}${randomPart(seed, 16)}`;
}

/** A 40-hex-char fake sha. */
export function sha(seed: string): string {
  let out = "";
  let i = 0;
  while (out.length < 40) out += fnv(`${seed}#${i++}`).toString(16).padStart(8, "0");
  return out.slice(0, 40);
}
