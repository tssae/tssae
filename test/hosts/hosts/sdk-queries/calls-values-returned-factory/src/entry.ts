import { getCallable } from "./defs.js";
const selected = getCallable();
selected(1);
getCallable()(2);
