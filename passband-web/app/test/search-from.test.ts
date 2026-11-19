// Ported from Tests/FromOperatorTests.swift, case for case.
import { describe, expect, test } from "vitest";
import {
  accepting,
  awaitingValue,
  fragment,
  isListing,
  listing,
  matches,
  searchOrder,
  senderFragment,
} from "../src/search/fromOperator";

describe("the menu opens on the trailing token", () => {
  test.each([
    ["from:", ""],
    ["from:d", "d"],
    ["from:dan@ex", "dan@ex"],
    ["wifi password from:abst", "abst"],
    ["From:Dan", "Dan"],
    ["FROM:", ""],
    ["", null],
    ["wifi", null],
    ["after:2026-01-01", null],
    ["fromage", null],
    ["from", null],
    ["xfrom:dan", null],
  ])("fragment(%j) = %j", (q, want) => expect(fragment(q)).toBe(want));
});

describe("a space closes it", () => {
  test.each([["from:dan "], ["from:dan@example.com  "], ["from:\n"], [" "]])("fragment(%j) = null", (q) =>
    expect(fragment(q)).toBeNull(),
  );
});

describe("an earlier operator is finished business", () => {
  test.each([
    ["from:dan wifi", null],
    ["from:dan from:", ""],
    ["from:dan after:2026 from:j", "j"],
  ])("fragment(%j) = %j", (q, want) => expect(fragment(q)).toBe(want));
});

describe("accepting", () => {
  test.each([
    ["dan@example.com", "from:d", "from:dan@example.com "],
    ["dan@example.com", "from:", "from:dan@example.com "],
    ["alex@northwind.example", "trip itinerary from:ale", "trip itinerary from:alex@northwind.example "],
    ["dan@example.com", "From:D", "from:dan@example.com "],
    ["j@example.com", "from:dan from:j", "from:dan from:j@example.com "],
  ])("completes the operator: %s in %j", (addr, q, want) => expect(accepting(addr, q)).toBe(want));

  test.each([
    ["dan@example.com", "wifi"],
    ["dan@example.com", "from:dan "],
    ["dan@example.com", ""],
  ])("outside the operator changes nothing: %s in %j", (addr, q) => expect(accepting(addr, q)).toBe(q));
});

describe("bare operators are not a search yet", () => {
  test.each([
    ["from:", true],
    ["From:", true],
    ["after:", true],
    ["from: before:", true],
    ["  from:  ", true],
    ["from:d", false],
    ["wifi from:", false],
    ["from:dan", false],
    ["after:2026-01-01", false],
    ["", false],
    ["   ", false],
    ["to:", true],
    ["in:sent", false],
    ["in:sent to:", false],
  ])("awaitingValue(%j) = %s", (q, want) => expect(awaitingValue(q)).toBe(want));
  test("scope and to: are not sender words", () => {
    expect(senderFragment("in:sent")).toBeNull();
    expect(senderFragment("to:dan")).toBeNull();
  });
});

describe("plain words ask the directory", () => {
  test.each([
    ["dan", "dan"],
    ["dan@ex", "dan@ex"],
    ["  Dan   Smith ", "Dan Smith"],
    ["d", null],
    ["", null],
    ["   ", null],
    ["from:dan", null],
    ["invoice from:dan", null],
    ["dan after:2026-01-01", null],
    ["From:", null],
    ["abstract conference wifi", "abstract conference wifi"],
    ["abstract conference wifi password", null],
  ])("senderFragment(%j) = %j", (q, want) => expect(senderFragment(q)).toBe(want));
});

describe("a held row must still match", () => {
  test.each([
    ["dan@example.com", "Dan Smith", "dan", true],
    ["d.smith@example.com", "Dan Smith", "DAN SM", true],
    ["billing@stripe.example", null, "Stripe", true],
    ["dan@example.com", "Dan Smith", "dana", false],
    ["billing@stripe.example", null, "dan", false],
  ])("matches(%s, %s, %s) = %s", (addr, name, frag, want) => expect(matches(addr, name, frag)).toBe(want));
});

describe("picking a sender lists their mail", () => {
  test("listing(of:)", () => {
    const q = listing("dan@example.com");
    expect(q).toBe("from:dan@example.com ");
    expect(isListing(q)).toBe(true);
    expect(senderFragment(q)).toBeNull();
    expect(fragment(q)).toBeNull();
  });
});

describe("operators alone are a listing", () => {
  test.each([
    ["from:dan@example.com", true],
    ["FROM:dan", true],
    ["from:dan after:2026-01-01", true],
    ["after:2026-01-01", true],
    ["invoice from:dan", false],
    ["dan", false],
    ["from:", false],
    ["from:dan before:", false],
    ["", false],
  ])("isListing(%j) = %s", (q, want) => expect(isListing(q)).toBe(want));
});

describe("search order follows the daemon", () => {
  test.each([
    ["wifi", false, true, false],
    ["from:dan@example.com", false, false, false],
    ["wifi", true, false, true],
    ["wifi from:dan", true, false, true],
    ["from:dan@example.com", true, false, false],
  ])("searchOrder(%j, plain %s)", (term, plain, unfinishedFirst, includeSent) =>
    expect(searchOrder(term, plain)).toEqual({ unfinishedFirst, includeSent }),
  );
});
