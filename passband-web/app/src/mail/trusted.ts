// THE ONLY HTML SINKS IN THE APP.
//
// The app runs under `require-trusted-types-for 'script'`, so handing a string
// to any HTML sink (innerHTML, iframe srcdoc, DOMParser, ...) throws unless it
// went through a named policy. There is exactly one: `passband-mail`, and it is
// used for exactly two things, both here:
//
//   - `parseMail`: reading a mail body into an INERT document (DOMParser runs
//     no script and fetches nothing) so `prepare()` can rewrite it;
//   - `setFrameDocument`: loading the document `prepare()` built into the
//     sandboxed reading frame.
//
// Anything else that wants to write HTML has to come through here, which is the
// point. The CSP names this policy and no other, so a second `createPolicy`
// anywhere in the app throws.

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

function trusted(html: string): string {
  const p = getPolicy();
  return (p ? p.createHTML(html) : html) as string;
}

/** Parse a mail body into an inert document. Nothing in it runs or loads. */
export function parseMail(html: string): Document {
  return new DOMParser().parseFromString(trusted(html), "text/html");
}

/** Load a prepared document into the reading frame. */
export function setFrameDocument(frame: HTMLIFrameElement, preparedDocument: string): void {
  (frame as unknown as { srcdoc: unknown }).srcdoc = trusted(preparedDocument);
}
