// Sender names, initials, avatar slots and favicon eligibility, ported from
// the Mac client's Lib/SenderIdentity.swift (`SenderID`).
//
// PRIVACY, as there: a human correspondent's domain is NEVER handed out for a
// favicon. Only robot and brand senders, which name a service rather than a
// person, are eligible (`eligibleFaviconDomain`).

/** Ten avatar slots, as `avatarSlots`. */
export const AVATAR_SLOTS = 10;

const isLetterOrNumber = (c: string) => /[\p{L}\p{N}]/u.test(c);

export interface ParsedSender {
  name: string;
  addr: string;
}

/** "Sarah Chen <sarah@acme.example>" -> { name: "Sarah Chen", addr: "sarah@acme.example" }. */
export function parseSender(sender: string): ParsedSender {
  const s = sender.trim();
  const open = s.lastIndexOf("<");
  const close = s.lastIndexOf(">");
  if (open >= 0 && close >= 0 && open < close) {
    const addr = s.slice(open + 1, close).trim();
    if (addr.includes("@")) {
      const name = s.slice(0, open).replace(/"/g, "").replace(/'/g, "").trim();
      return { name, addr };
    }
  }
  const space = s.lastIndexOf(" ");
  if (space >= 0) {
    const tail = s.slice(space + 1);
    if (tail.includes("@") && !tail.includes(" ")) {
      const name = s.slice(0, space).replace(/"/g, "").replace(/'/g, "").trim();
      return { name, addr: tail };
    }
  }
  if (s.includes("@")) return { name: "", addr: s };
  return { name: s, addr: s };
}

/** The lowercased bare address: the grouping key. */
export function senderAddress(sender: string): string {
  return parseSender(sender).addr.toLowerCase();
}

/** The mailbox before the "@", "+tag" dropped. */
export function localPart(addr: string): string {
  const local = addr.split("@").filter((x) => x.length > 0)[0] ?? "";
  return local.split("+").filter((x) => x.length > 0)[0] ?? local;
}

/** A row's sender string, as `senderString`: "Name <addr>" or the address. */
export function senderString(addr: string, name?: string | null): string {
  return name ? `${name} <${addr}>` : addr;
}

function capitalizingFirst(s: string): string {
  const first = Array.from(s)[0];
  return first === undefined ? s : first.toUpperCase() + s.slice(first.length);
}

const ROBOT_LOCALS = new Set([
  "no-reply", "noreply", "do-not-reply", "donotreply", "notification", "notifications",
  "alert", "alerts", "update", "updates", "news", "newsletter", "marketing", "mailer",
  "billing", "receipt", "receipts", "order", "orders", "team", "hello", "info", "support",
  "account", "accounts", "security", "admin", "service", "contact", "help", "feedback",
  "mail", "email", "invoice", "invoices", "statement", "statements", "confirmation",
  "confirmations", "tracking", "delivery", "digest", "bulletin",
]);

const COMPOUND_SUFFIXES = new Set([
  "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "ltd.uk", "plc.uk",
  "com.au", "net.au", "org.au", "edu.au", "gov.au",
  "co.jp", "or.jp", "ne.jp", "ac.jp",
  "co.nz", "co.za", "co.in", "co.kr", "co.il", "co.th",
  "com.br", "com.mx", "com.sg", "com.tr", "com.cn", "com.hk", "com.tw",
  "com.ar", "com.co", "com.pl", "com.ua",
]);

const ROBOT_MARKERS = ["noreply", "donotreply", "mailerdaemon", "automailer", "automated", "autoconfirm"];

const CONSUMER_HOSTS = new Set([
  "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com",
  "yahoo.com", "ymail.com", "aol.com", "icloud.com", "me.com", "mac.com",
  "proton.me", "protonmail.com", "pm.me", "gmx.com", "gmx.net", "mail.com",
  "zoho.com", "fastmail.com", "hey.com", "yandex.com", "qq.com", "163.com",
  "126.com", "naver.com", "web.de", "t-online.de",
]);

export function isRobot(sender: string): boolean {
  const base = localPart(parseSender(sender).addr).toLowerCase();
  if (ROBOT_LOCALS.has(base)) return true;
  const squashed = Array.from(base).filter(isLetterOrNumber).join("");
  return ROBOT_MARKERS.some((m) => squashed.includes(m));
}

/** The registrable domain, for a favicon lookup (two labels, three under a compound suffix). */
export function faviconDomain(sender: string): string | null {
  const parts = parseSender(sender).addr.split("@").filter((x) => x.length > 0);
  if (parts.length < 2) return null;
  let host = parts[parts.length - 1]!.toLowerCase();
  while (host.endsWith(".")) host = host.slice(0, -1);
  if (!host.includes(".")) return null;
  const labels = host.split(".").filter((x) => x.length > 0);
  if (labels.length < 2) return null;
  const lastTwo = labels.slice(-2).join(".");
  if (labels.length >= 3 && COMPOUND_SUFFIXES.has(lastTwo)) return labels.slice(-3).join(".");
  return lastTwo;
}

function baseLabel(sender: string): string | null {
  const domain = faviconDomain(sender);
  if (!domain) return null;
  const first = domain.split(".")[0] ?? "";
  return first === "" ? null : first;
}

/** "stripe" -> "Stripe", "t-mobile" -> "T-Mobile". */
export function brandLabel(sender: string): string | null {
  const base = baseLabel(sender);
  if (!base) return null;
  return base.split("-").map(capitalizingFirst).join("-");
}

function nameTokens(s: string): string[] {
  return s
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .split(/[\s\-._]+/u)
    .map((t) => Array.from(t).filter(isLetterOrNumber).join(""))
    .filter((t) => t.length > 0);
}

const sameTokens = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** The sender presents itself AS the brand: local-part = label, or display name = domain word for word. */
export function isBrand(sender: string): boolean {
  const parsed = parseSender(sender);
  const domain = faviconDomain(sender);
  if (!domain) return false;
  const base = domain.split(".")[0] ?? "";
  if (!base) return false;
  const local = localPart(parsed.addr);
  if (local && local.toLowerCase() === base) return true;
  if (CONSUMER_HOSTS.has(domain)) return false;
  const name = nameTokens(parsed.name);
  if (name.length === 0) return false;
  return sameTokens(name, nameTokens(base)) || sameTokens(name, nameTokens(domain));
}

function serviceLabel(sender: string): string | null {
  if (isBrand(sender)) {
    const local = localPart(parseSender(sender).addr);
    if (local) return local;
  }
  if (isRobot(sender)) {
    const label = brandLabel(sender);
    if (label) return label;
  }
  return null;
}

/** The name a ROW shows: a real display name, else a service label, else the address. */
export function displayName(sender: string): string {
  const p = parseSender(sender);
  if (p.name && p.name.toLowerCase() !== p.addr.toLowerCase()) return p.name;
  return serviceLabel(sender) ?? p.addr;
}

/** Up to two initials. Never from the full address (the domain would leak in). */
export function initials(sender: string): string {
  const p = parseSender(sender);
  const local = localPart(p.addr);
  let source = p.name;
  if (!source) {
    const shown = displayName(sender);
    source = shown && shown.toLowerCase() !== p.addr.toLowerCase() ? shown : local;
  }
  const words = source
    .split(/[ ._-]/)
    .filter((w) => w.length > 0 && Array.from(w).some(isLetterOrNumber));
  const first = (w: string) => Array.from(w)[0] ?? "";
  if (words.length >= 2) return (first(words[0]!) + first(words[1]!)).toUpperCase();
  if (words.length === 1 && Array.from(words[0]!).length >= 2) {
    return Array.from(words[0]!).slice(0, 2).join("").toUpperCase();
  }
  const fallback = Array.from(local)[0] ?? Array.from(source)[0] ?? "?";
  return fallback.toUpperCase();
}

/** djb2 over the lowercased address's code points, in 32-bit wrapping arithmetic. */
function hashAddr(sender: string): number {
  const a = parseSender(sender).addr.toLowerCase();
  let h = 5381;
  for (const ch of a) {
    h = (Math.imul(h, 33) + ch.codePointAt(0)!) >>> 0;
  }
  return h;
}

/** Palette slot 0..9, deterministic by address. */
export function avatarSlot(sender: string): number {
  return hashAddr(sender) % AVATAR_SLOTS;
}

/** A domain safe to fetch a favicon for: robots and brands only, never a person's. */
export function eligibleFaviconDomain(sender: string): string | null {
  return isRobot(sender) || isBrand(sender) ? faviconDomain(sender) : null;
}

