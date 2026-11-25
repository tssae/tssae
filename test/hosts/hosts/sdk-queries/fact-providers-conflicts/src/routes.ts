export function registerRoute(path: string, handler: () => void) {
  return { path, handler };
}
export const usersRoute = registerRoute("/users", () => {});
