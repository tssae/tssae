import { deliver } from "./sink.js";

/**
 * @param {"alpha" | "beta"} mode
 * @param {string} payload
 * @returns {string}
 */
export function forwardBridge(mode, payload) {
  const local = payload;
  return deliver(mode, local);
}
