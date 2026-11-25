import { forward, genericTarget, getFactory, keepIdentity } from "./library.js";
const factory = getFactory();
factory(1);
getFactory()(2);
const generic = keepIdentity(genericTarget);
generic(3);
export const entryValue = forward(4);
