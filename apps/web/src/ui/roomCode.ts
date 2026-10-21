/**
 * Crockford base32 room codes (task T-047).
 *
 * The alphabet has 32 symbols and no I, L, O or U, so a spoken or handwritten
 * code cannot turn into a different room. Input is normalised before it is
 * compared: case is ignored, hyphens are dropped, and I/L become 1 while O
 * becomes 0.
 */

export const ROOM_CODE_LENGTH = 6;
export const ROOM_CODE_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const ALIASES: Record<string, string> = {
  I: "1",
  L: "1",
  O: "0",
};

/** Uppercase, strip separators and resolve Crockford lookalikes. */
export function normaliseRoomCode(value: string): string {
  const stripped = value.toUpperCase().replace(/[\s-]/g, "");
  let out = "";
  for (const character of stripped) {
    out += ALIASES[character] ?? character;
  }
  return out;
}

/** True only for a canonical six-character Crockford code. */
export function isValidRoomCode(value: string): boolean {
  const code = normaliseRoomCode(value);
  if (code.length !== ROOM_CODE_LENGTH) return false;
  for (const character of code) {
    if (!ROOM_CODE_ALPHABET.includes(character)) return false;
  }
  return true;
}

/** Generate a code. A caller-supplied source makes tests deterministic. */
export function generateRoomCode(random?: () => Uint8Array): string {
  const bytes = random?.() ?? secureBytes(ROOM_CODE_LENGTH);
  let code = "";
  for (let i = 0; i < ROOM_CODE_LENGTH; i += 1) {
    code += ROOM_CODE_ALPHABET[bytes[i % bytes.length] & 0x1f];
  }
  return code;
}

/**
 * Ask the registry until it returns an unused code. The registry is the only
 * authority on collisions; the browser never assumes a generated code exists.
 */
export async function createUniqueCode(
  exists: (code: string) => Promise<boolean>,
  attempts = 8,
): Promise<string> {
  for (let i = 0; i < attempts; i += 1) {
    const code = generateRoomCode();
    if (!(await exists(code))) return code;
  }
  throw new Error("could not allocate a room code");
}

function secureBytes(length: number): Uint8Array {
  const bytes = new Uint8Array(length);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < length; i += 1) {
      bytes[i] = Math.floor(Math.random() * 256);
    }
  }
  return bytes;
}
