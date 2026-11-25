declare function Item(): unknown;

export function View() {
  return (
    <div>
      {/* verule-ignore-next-code-line r-jsx -- JSX comment */}
      <Item />
    </div>
  );
}
