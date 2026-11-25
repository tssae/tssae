import { forward, genericTarget, getFactory } from "./library.js";
getFactory()(5);
genericTarget(6);
export const otherValue = forward(7);
