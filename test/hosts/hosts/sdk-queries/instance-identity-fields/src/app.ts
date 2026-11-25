import {
  createHolder,
  getItem,
  makeGetter,
  Manager,
  setItem,
} from "./holder.js";
import {
  createResource,
  getSingletonA,
  getSingletonB,
  Store,
} from "./resource.js";

function cleanup(_target: Store): void {}

export function exerciseFieldsAndClosures(): void {
  const a = createResource();
  const b = createResource();
  const c = createResource();

  const h1 = createHolder();
  const h2 = createHolder();
  setItem(h1, a);
  h1.other = b;
  setItem(h2, c);

  const readH1Item = getItem(h1);
  const readH1Other = h1.other;
  const readH2Item = getItem(h2);

  const m1 = new Manager(a);
  const m2 = new Manager(b);
  const m1StoreBefore = m1.getStore();
  const m1StoreAgain = m1.getStore();
  const m1Other = m1.getOther();
  const m2Store = m2.getStore();

  m1.setStore(c);
  const m1StoreAfter = m1.getStore();

  const runClosure = () => {
    const inner = a;
    cleanup(inner);
  };

  const g1 = makeGetter();
  const g2 = makeGetter();
  const g1First = g1();
  const g1Second = g1();
  const g2First = g2();

  const sing1 = getSingletonA();
  const sing2 = getSingletonB();
  void [
    readH1Item,
    readH1Other,
    readH2Item,
    m1StoreBefore,
    m1StoreAgain,
    m1Other,
    m2Store,
    m1StoreAfter,
    runClosure,
    g1First,
    g1Second,
    g2First,
    sing1,
    sing2,
  ];
}
