// THE ONLY HTML SINK IN THE APP.
//
// The app runs under `require-trusted-types-for 'script'`, so assigning a
// string to any HTML sink (innerHTML, iframe srcdoc, ...) throws unless it went
// through a named policy. There is exactly one: `passband-mail`, and the only
// thing allowed through it is a document built by `prepare()` for the
// sandboxed reading frame. Anything else that wants to write HTML has to come
// through here, which is the point.

interface TTPolicy {
  createHTML(input: string): unknown;
}
interface TTFactory {
  createPolicy(name: string, rules: { createHTML(input: string): string }): TTPolicy;
}

let policy: TTPolicy | null | undefined;

function getPolicy(): TTPolicy | null {
  if (policy !== undefined) return policy;
  const tt = (globalThis as { trustedTypes?: TTFactory }).trustedTypes;
  // Browsers without Trusted Types fall back to the plain string; the
  // frame's sandbox and CSP still hold there.
  policy = tt ? tt.createPolicy("passband-mail", { createHTML: (s) => s }) : null;
  return policy;
}

/** Load a prepared document into the reading frame. */
export function setFrameDocument(frame: HTMLIFrameElement, preparedDocument: string): void {
  const p = getPolicy();
  (frame as unknown as { srcdoc: unknown }).srcdoc = p ? p.createHTML(preparedDocument) : preparedDocument;
}
