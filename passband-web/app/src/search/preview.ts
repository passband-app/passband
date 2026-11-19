// A hit's excerpt, made readable (the Mac's Lib/SearchPreview.swift): URLs
// become their domain plus any readable path words, stopping at tracking and
// redirect IDs; a "Manage subscription" footer is cut; whitespace collapses.
// Display only — the message itself is untouched.

const URLS = /(?:https?:\/\/|www\.|\b(?:[a-z0-9-]+\.)+[a-z]{2,24}\/)[^\s<>]+/gi;
const TRIM = ".,;!)]}…";

function trimSet(s: string): string {
  let a = 0;
  let b = s.length;
  while (a < b && TRIM.includes(s[a]!)) a++;
  while (b > a && TRIM.includes(s[b - 1]!)) b--;
  return s.slice(a, b);
}

function readable(raw: string): string {
  const url = trimSet(raw);
  const suffix = raw.slice(url.length);
  const address = url.toLowerCase().startsWith("http") ? url : `https://${url}`;
  let host: string;
  let path: string;
  try {
    const u = new URL(address);
    host = u.hostname;
    path = u.pathname;
  } catch {
    return `…${suffix}`;
  }
  if (!host) return `…${suffix}`;
  const domain = host.startsWith("www.") ? host.slice(4) : host;
  const words: string[] = [];
  for (const enc of path.split("/").filter((s) => s.length > 0)) {
    let value: string;
    try {
      value = decodeURIComponent(enc);
    } catch {
      break;
    }
    const n = [...value].length;
    const ok =
      n > 1 &&
      n <= 48 &&
      /\p{L}/u.test(value) &&
      /^[\p{L}\-_ .]+$/u.test(value) &&
      (n <= 24 || value.includes("-") || value.includes(" "));
    if (!ok) break;
    words.push(value.replace(/_/g, "-"));
  }
  return domain + (words.length ? `/${words.join("/")}` : "") + suffix;
}

export function cleanPreview(text: string): string {
  let cleaned = text.replace(URLS, (m) => readable(m));
  const footer = cleaned.search(/manage subscription/i);
  if (footer >= 0) cleaned = cleaned.slice(0, footer);
  return cleaned.replace(/\s+/g, " ").trim();
}
