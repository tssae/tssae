interface Owner {
  id: number;
  email: string;
}
interface Workspace {
  id: number;
  plan: string;
  owner: Owner;
}
interface PageRow {
  id: number;
  slug: string;
  customDomain: string | null;
  workspace: Workspace;
}
declare function fetchPage(): PageRow;
declare function sink(tag: string, value: unknown): void;

export function run(): void {
  const page = fetchPage();
  sink("direct", page);
  sink("dotProp", page.workspace);
  sink("bracketProp", page["workspace"]);
  const { workspace } = page;
  sink("destructuredSame", workspace);
  const { workspace: ws } = page;
  sink("destructuredAlias", ws);
  sink("nestedOwner", ws.owner);
  const { slug, ...rest1 } = page;
  sink("rest1", rest1);
  const { customDomain, ...rest2 } = rest1;
  sink("rest2", rest2);
  sink("omittedSlugAccess", page.slug);
  sink("survivingWsAccess", rest1.workspace);
  const { plan, ...wsRest } = page.workspace;
  sink("nestedRest", wsRest);
  const wrapped = { w: page.workspace };
  sink("wrappedMember", wrapped.w);
  void slug;
  void customDomain;
  void plan;
}
