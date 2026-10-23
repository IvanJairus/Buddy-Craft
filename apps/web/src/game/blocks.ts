/**
 * Block ids the client needs by name. The registry order is frozen by
 * `data/blocks.ron`; only the crate is placeable and breakable in v1.
 */

export const BLOCK_AIR = 0;
export const BLOCK_CRATE = 11;

export function isBreakable(block: number): boolean {
  return block === BLOCK_CRATE;
}
