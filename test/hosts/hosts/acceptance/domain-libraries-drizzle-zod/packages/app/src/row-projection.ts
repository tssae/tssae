import {
  gateFieldsSchema,
  getCompleteUser,
  getDynamicUser,
  getPageWithWorkspace,
  getUserBrief,
  selectWorkspaceSchema,
  strictUserSchema,
  userSchema,
} from "@acceptance-host/db";

export async function checkUserRows(externalUser: unknown = {}) {
  const complete = await getCompleteUser();
  const brief = await getUserBrief();
  const dynamic = await getDynamicUser();
  const unrelated = userSchema.parse(externalUser);
  void strictUserSchema;

  return {
    complete: complete === undefined ? undefined : userSchema.parse(complete),
    defaulted: brief === undefined ? undefined : userSchema.parse(brief),
    dynamic: dynamic === undefined ? undefined : userSchema.parse(dynamic),
    unrelated,
  };
}

export async function checkPageAndRest() {
  const pageRow = await getPageWithWorkspace();
  if (pageRow === undefined) {
    return null;
  }
  const ws = selectWorkspaceSchema.safeParse(pageRow.workspace);
  const { workspace: _ws, ...restPage } = pageRow;
  const gate = gateFieldsSchema.parse(restPage);
  return { ws, gate };
}
