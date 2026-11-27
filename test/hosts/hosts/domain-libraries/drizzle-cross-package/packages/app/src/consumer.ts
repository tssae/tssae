import { accounts } from "@acme/data";

function registerTable(_table: unknown): void {}
function registerOther(_value: unknown): void {}
const notATable = { label: "hello" };

export function init() {
  registerTable(accounts);
  registerOther(notATable);
}
