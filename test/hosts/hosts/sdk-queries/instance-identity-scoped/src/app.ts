import { forward } from "./forward.js";
import { createStore, Store } from "./resource.js";
export const appValue = forward(createStore());
export const appFactory = createStore();
void Store;
