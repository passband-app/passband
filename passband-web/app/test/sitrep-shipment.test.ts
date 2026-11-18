// Ported from passband/Tests/ShipmentWireTests.swift (18 checks), plus the
// card's status and ETA words (Views/SitrepZones.swift, ShipmentCard).
import { describe, expect, test } from "vitest";
import { shipment, type Shipment } from "../src/api/wire";
import { parseInstant } from "../src/lib/time";
import { displayTitle, etaText, orderLine, staysOnRail, statusText } from "../src/sitrep/shipments";

const base = {
  id: 7,
  account_id: 1,
  tracking_number: "1Z999AA10123456784",
  carrier: "ups",
  status: "shipped",
  first_seen: "2026-09-20T10:00:00Z",
  last_update: "2026-09-22T10:00:00Z",
};
const decode = (extra: object): Shipment | null => shipment({ ...base, ...extra });

describe("ShipmentWireTests", () => {
  test("an old row decodes without the order fields", () => {
    const row = decode({ item_name: "" });
    expect(row).not.toBeNull();
    expect(row!.merchant).toBeUndefined();
    expect(row!.orders).toBeUndefined();
    expect(row!.legs).toBeUndefined();
    expect(displayTitle(row!)).toBe("Package");
    expect(orderLine(row!)).toBeNull();
  });

  test("a grouped row carries orders and legs", () => {
    const row = decode({
      item_name: "Trackside Muffler Insert TM10 x 1",
      merchant: "Rivera Exhaust Works",
      orders: [{ merchant: "Rivera Exhaust Works", order_ref: "50417" }],
      legs: [
        {
          id: 23,
          carrier: "ups",
          tracking_number: "1Z999AA10123456795",
          status: "delivered",
          tracking_url: null,
          last_update: "2026-09-18T10:00:00Z",
          delivered_at: "2026-09-18T09:00:00Z",
        },
      ],
    })!;
    expect(row.legs?.[0]?.status).toBe("delivered");
    expect(row.legs?.[0]?.id).toBe(23);
    expect(displayTitle(row)).toBe("Trackside Muffler Insert TM10 x 1");
    expect(orderLine(row)).toBe("Rivera Exhaust Works · #50417 · 2 packages");
  });

  test("an unknown leg carrier or status falls back instead of sinking the row", () => {
    const row = decode({
      item_name: "x",
      legs: [{ id: 1, carrier: "ontrac", tracking_number: "T", status: "in_orbit", last_update: "2026-09-18T10:00:00Z" }],
    })!;
    expect(row.legs?.[0]?.carrier).toBe("unknown");
    expect(row.legs?.[0]?.status).toBe("ordered");
  });

  test("the title falls back to the merchant, emoji stripped, then Package", () => {
    const row = decode({
      item_name: "",
      merchant: "  🎁 Rivera   Exhaust Works ",
      orders: [{ merchant: null, order_ref: "50417" }],
    })!;
    expect(displayTitle(row)).toBe("Rivera Exhaust Works");
    expect(orderLine(row)).toBe("#50417");
  });

  test("an order ref gets exactly one hash", () => {
    const row = decode({
      item_name: "x",
      orders: [
        { merchant: null, order_ref: "#1001" },
        { merchant: null, order_ref: "1002" },
      ],
    })!;
    expect(orderLine(row)).toBe("#1001, #1002");
  });

  test("a card stays while any package is coming", () => {
    const never = () => false;
    const landed = { status: "delivered", delivered_at: "2026-09-20T10:00:00Z" };
    const leg = (status: string) => ({
      id: 8,
      carrier: "ups",
      tracking_number: "1Z999AA10123456795",
      status,
      last_update: "2026-09-19T10:00:00Z",
    });
    expect(staysOnRail(decode({ item_name: "x", ...landed, legs: [leg("shipped")] })!, never)).toBe(true);
    const done = decode({ item_name: "x", ...landed, legs: [leg("delivered")] })!;
    expect(staysOnRail(done, never)).toBe(false);
    expect(staysOnRail(done, () => true)).toBe(true);
    expect(staysOnRail(decode({ item_name: "x", status: "shipped" })!, never)).toBe(true);
  });
});

describe("the card's words", () => {
  const LA = "America/Los_Angeles";
  // Wednesday 2026-09-23, 10:00 in Los Angeles.
  const now = parseInstant("2026-09-23T17:00:00Z")!;

  test("ETA: a weekday inside the window, the date past it, none on the day it is out", () => {
    expect(etaText(decode({ item_name: "x", eta: "2026-09-26" })!, now, LA)).toBe("arrives Sat");
    expect(etaText(decode({ item_name: "x", eta: "2026-10-15" })!, now, LA)).toBe("arrives Oct 15");
    expect(etaText(decode({ item_name: "x", eta: "2026-09-23" })!, now, LA)).toBe("arrives today");
    expect(etaText(decode({ item_name: "x", status: "out_for_delivery", eta: "2026-09-23" })!, now, LA)).toBeNull();
    expect(etaText(decode({ item_name: "x", eta: "2026-09-20" })!, now, LA)).toBe("expected Sep 20");
    expect(etaText(decode({ item_name: "x", status: "delivered", eta: "2026-09-26" })!, now, LA)).toBeNull();
  });

  test("delivered says when: a clock time today, a weekday this week", () => {
    expect(statusText(decode({ item_name: "x", status: "delivered", delivered_at: "2026-09-23T16:10:00Z" })!, now, LA)).toBe(
      "delivered 9:10 AM",
    );
    expect(statusText(decode({ item_name: "x", status: "delivered", delivered_at: "2026-09-21T16:10:00Z" })!, now, LA)).toBe(
      "delivered Mon",
    );
    expect(statusText(decode({ item_name: "x", status: "out_for_delivery" })!, now, LA)).toBe("out for delivery");
  });
});
