/** Only http(s) leaves the app, in a new tab that cannot reach back or see where it came from. */
export function safeLinkTarget(raw: string): string | null {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.protocol !== "https:" && u.protocol !== "http:") return null;
  if (u.username || u.password) return null;
  return u.toString();
}

export function openLink(raw: string): void {
  const target = safeLinkTarget(raw);
  if (target) window.open(target, "_blank", "noopener,noreferrer");
}
