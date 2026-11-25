import { computeUnique, keepIdentity } from "./library.js";
const selected = keepIdentity(computeUnique);
selected(1);
