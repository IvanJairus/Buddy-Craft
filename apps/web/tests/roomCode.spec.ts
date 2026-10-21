/**
 * T-047 room-code unit specs. These are pure Node tests; the registry HTTP
 * endpoints live behind the Cloudflare worker.
 */

import { expect, test } from "@playwright/test";
import {
  ROOM_CODE_ALPHABET,
  createUniqueCode,
  generateRoomCode,
  isValidRoomCode,
  normaliseRoomCode,
} from "../src/ui/roomCode";

test("codes use only Crockford base32 and six symbols", () => {
  const code = generateRoomCode(() => new Uint8Array([0, 1, 31, 18, 7, 9]));
  expect(code).toHaveLength(6);
  for (const character of code) {
    expect(ROOM_CODE_ALPHABET).toContain(character);
  }
  expect(code).not.toMatch(/[ILOU]/);
});

test("lookalikes and separators normalise before comparison", () => {
  expect(normaliseRoomCode(" ab-c1io ")).toBe("ABC110");
  expect(isValidRoomCode("AB-C1IO")).toBe(true);
  expect(isValidRoomCode("ABC1I")).toBe(false);
  expect(isValidRoomCode("ABC1!O")).toBe(false);
});

test("the allocator refuses to loop forever on collisions", async () => {
  await expect(createUniqueCode(async () => true, 3)).rejects.toThrow(/room code/);
  const code = await createUniqueCode(async () => false, 3);
  expect(isValidRoomCode(code)).toBe(true);
});
