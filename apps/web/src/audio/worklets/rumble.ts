/**
 * AudioWorklet entry point for the 55 Hz base-under-attack rumble (T-049).
 *
 * The processor is plain JavaScript in `public/audio/rumble.worklet.js` so it
 * can be loaded by `audioWorklet.addModule` without a bundler step. This module
 * owns the URL and processor name that the engine and tests share.
 */

export const RUMBLE_WORKLET_URL = "/audio/rumble.worklet.js";
export const RUMBLE_PROCESSOR = "cb-rumble";
