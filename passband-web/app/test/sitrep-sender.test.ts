// Ported from passband/Tests/SenderIdentityTests.swift: the row rules (brand
// and robot recognition, the favicon boundary, display names, initials). The
// notification-name rule (`readableName`) has no web caller and is not ported.
import { describe, expect, test } from "vitest";
import { avatarSlot, displayName, eligibleFaviconDomain, initials, isBrand, isRobot, parseSender } from "../src/sitrep/sender";

describe("brands are recognised", () => {
  test.each([
    ["eBay <eBay@eBay.com>", "local-part equal to the label"],
    ["ebay@ebay.com", "the same with no display name at all"],
    ["Airbnb <express@airbnb.com>", "display name over a routing box"],
    ["STRIPE <e@stripe.com>", "case is not part of the match"],
    ["Booking.com <news-2938@booking.com>", "a name carrying its TLD"],
    ["Blue Apron <hi@blue-apron.com>", "two words, one hyphen"],
    ["T-Mobile <news@t-mobile.com>", "hyphens on both sides"],
    ["Marks and Spencer <news@marks-and-spencer.co.uk>", "three words under a compound suffix"],
    ["Nestlé <news@nestle.com>", "diacritics fold before matching"],
  ])("%s (%s)", (sender) => {
    expect(isBrand(sender)).toBe(true);
  });
  test("the robot arm", () => {
    expect(isRobot("no-reply@stripe.com")).toBe(true);
    expect(isRobot('"Chase" <no.reply.alerts@chase.com>')).toBe(true);
  });
});

describe("people are not", () => {
  test.each([
    ["Sarah Chen <sarah@acme.com>", "a person at a company"],
    ["sarah@acme.com", "a bare human address"],
    ["Jane Doe <jane@janedoe.com>", "a person on their own domain"],
    ["Bob Smith <bob@bobsmith.com>", "and again, concatenated"],
    ["Sarah Chen <sarah@sarahchen.co.uk>", "and under a compound suffix"],
    ["Gmail <friend@gmail.com>", "a consumer host is never the brand"],
    ["Proton <friend@proton.me>", "nor a privacy host"],
    ["Sarah <sarah@gmail.com>", "a first name on a consumer host"],
    ["Samuel Smith <ssmith@sam.com>", "no prefix matching"],
    ["Airbnb Support <express@airbnb.com>", '"Airbnb Support" is not'],
    ["Airbnb, Inc. <express@airbnb.com>", 'nor "Airbnb, Inc."'],
    ["sarah@acme.com <sarah@acme.com>", "the address is not a name"],
  ])("%s (%s)", (sender) => {
    expect(isBrand(sender)).toBe(false);
  });
});

describe("the known remainder, as on the Mac", () => {
  test("a one-word surname on its own domain still reads as a brand", () => {
    expect(isBrand("Anderson <john@anderson.com>")).toBe(true);
    expect(eligibleFaviconDomain("Chen <sc@chen.com>")).toBe("chen.com");
  });
});

describe("the favicon boundary holds", () => {
  test("services resolve to their registrable domain; people never do", () => {
    expect(eligibleFaviconDomain("Airbnb <express@airbnb.com>")).toBe("airbnb.com");
    expect(eligibleFaviconDomain("no-reply@stripe.com")).toBe("stripe.com");
    expect(eligibleFaviconDomain("Sarah Chen <sarah@acme.com>")).toBeNull();
    expect(eligibleFaviconDomain("Samuel Smith <ssmith@sam.com>")).toBeNull();
    expect(eligibleFaviconDomain("Jane Doe <jane@janedoe.com>")).toBeNull();
    expect(eligibleFaviconDomain("express@airbnb.com")).toBeNull();
    expect(eligibleFaviconDomain("Bob")).toBeNull();
    expect(eligibleFaviconDomain("no-reply@login.awsapps.com")).toBe("awsapps.com");
  });
});

describe("names stay correct", () => {
  test("display names", () => {
    expect(displayName("Airbnb <express@airbnb.com>")).toBe("Airbnb");
    expect(displayName("ebay@ebay.com")).toBe("ebay");
    expect(displayName("no-reply@stripe.com")).toBe("Stripe");
    expect(displayName("sarah@acme.com")).toBe("sarah@acme.com");
    expect(displayName("no-reply@t-mobile.com")).toBe("T-Mobile");
    expect(displayName("bounce-1234-5678@em.brand.com")).toBe("bounce-1234-5678@em.brand.com");
  });
  test("initials never take the domain's letters", () => {
    expect(initials("Sarah Chen <sarah@acme.com>")).toBe("SC");
    expect(initials("jrivera97@gmail.com")).toBe("JR");
  });
});

describe("parse and slot", () => {
  test("the last angle-bracket pair, quotes dropped", () => {
    expect(parseSender('"Doe, John" <j@x.example>')).toEqual({ name: "Doe, John", addr: "j@x.example" });
    expect(parseSender("Name a@b.example")).toEqual({ name: "Name", addr: "a@b.example" });
    expect(parseSender("a@b.example")).toEqual({ name: "", addr: "a@b.example" });
  });
  test("the avatar slot is the djb2 hash of the lowercased address, mod 10", () => {
    // djb2("a@b.c") computed by hand: stable across sessions and case.
    let h = 5381;
    for (const ch of "a@b.c") h = (Math.imul(h, 33) + ch.codePointAt(0)!) >>> 0;
    expect(avatarSlot("A@B.C")).toBe(h % 10);
    expect(avatarSlot("Someone <a@b.c>")).toBe(avatarSlot("a@b.c"));
  });
});
