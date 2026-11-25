declare function BadWidget(props: { label: string }): string;
declare function GoodWidget(props: { label: string }): unknown;

/**
 * Example documentation:
 * // verule-ignore-next-code-line no-bad-widget -- Example inside JSDoc.
 */
/** verule-ignore-next-code-line no-bad-widget -- Leading JSDoc text must not act as directive. */
export function View() {
  const doc = "// verule-ignore-next-code-line no-bad-widget -- inside string";
  const tmpl = `
  // verule-ignore-next-code-line no-bad-widget -- inside template literal
  ${
    // verule-ignore-next-code-line no-bad-widget -- inside template interpolation
    BadWidget({ label: doc })
  }`;
  void tmpl;
  return (
    <div>
      {/* verule-ignore-next-code-line no-bad-widget -- Valid JSX child ignore. */}
      <BadWidget label="suppressed" />
      {/* verule-ignore-next-code-line no-bad-widget --
          Blank line after JSX directive truncates match. */}

      <BadWidget label="after-blank" />
      <GoodWidget label="ok" />
    </div>
  );
}

export function nonStandalone(): void {
  BadWidget({ label: "trailing" }); // verule-ignore-next-code-line no-bad-widget -- trailing comment ignored
  /* verule-ignore-next-code-line no-bad-widget -- inline before code */ BadWidget(
    { label: "inline" },
  );
}
