// A synthetic sitrep for the design harness. Every person and company is
// invented. Not shipped: only the e2e build's harness page imports it.
import type { Sitrep } from "../src/sitrep/model";

export const SAMPLE: Sitrep = {
  greeting: "Good morning, Sam",
  headline: "The offsite has a date and your keyboard lands today.",
  eyes: [
    {
      id: 1,
      threadId: "t1",
      sender: "Parkline Properties",
      initials: "PP",
      avatarHue: 4,
      line: "Your lease renewal still needs a signature before the end of the month.",
      pill: { text: "3d past due", tone: "overdue" },
      hasAttachment: true,
      overdue: true,
    },
    {
      id: 2,
      threadId: "t2",
      sender: "Dr. Ortiz's Office",
      initials: "DO",
      avatarHue: 1,
      line: "Confirm Thursday's 2:30 appointment or call to reschedule.",
      pill: { text: "due today", tone: "due" },
      hasAttachment: false,
      overdue: false,
    },
    {
      id: 3,
      threadId: "t3",
      sender: "Jamie Chen",
      initials: "JC",
      avatarHue: 0,
      line: "Needs your pick between two offsite venues so they can hold the date.",
      pill: { text: "due Fri", tone: "due" },
      hasAttachment: false,
      overdue: false,
    },
    {
      id: 4,
      threadId: "t4",
      sender: "Alex Rivera",
      initials: "AR",
      avatarHue: 3,
      line: "Replied to your offsite dates: the 14th works for everyone.",
      hasAttachment: false,
      overdue: false,
    },
    {
      id: 5,
      threadId: "t5",
      sender: "Mom",
      initials: "M",
      avatarHue: 6,
      line: "Sent photos from the weekend.",
      hasAttachment: true,
      overdue: false,
    },
  ],
  reading: [
    { key: "a", sender: "Cats Weekly", count: 4, snippet: "Why your cat stares at the wall, and nine other mysteries.", threadId: "r1", initials: "CW", avatarHue: 8 },
    { key: "b", sender: "The Federal Observer", count: 3, snippet: "Markets react calmly to everything. Plus the week ahead.", threadId: "r2", initials: "FO", avatarHue: 5 },
    { key: "c", sender: "Haightside Hall", count: 2, snippet: "This month's programme: three nights of new music.", threadId: "r3", initials: "HH", avatarHue: 7 },
    { key: "d", sender: "Brightly", count: 9, snippet: "48 hours only: 40% off everything you saved.", threadId: "r4", initials: "B", avatarHue: 4 },
    { key: "e", sender: "Rainforest", count: 5, snippet: "Deals picked for you, based on one search you made.", threadId: "r5", initials: "R", avatarHue: 2 },
    { key: "f", sender: "Skyward Miles", count: 2, snippet: "1,204 miles expire at the end of the month.", threadId: "r6", initials: "SM", avatarHue: 9 },
  ],
  calendar: [
    { key: "c1", title: "Dr. Ortiz · checkup", when: "Thu 2:30 PM", threadId: "t2" },
    { key: "c2", title: "Team offsite", when: "Oct 14", pill: { text: "updated", tone: "neutral" } },
  ],
  shipments: [
    { key: "s1", title: "Keyplane Q1 keyboard", detail: "Keyplane · order KP-48211", status: { text: "out for delivery", tone: "due" } },
    { key: "s2", title: "Linen notebooks, set of 3", detail: "Rainforest · 2 items", status: { text: "shipped", tone: "accent" }, eta: "arrives Sat" },
  ],
  billing: [
    { key: "b1", payee: "Metro Power & Light", pill: { text: "autopay", tone: "neutral" }, amount: "$142.08", overdue: false },
    { key: "b2", payee: "Northbank ··4417", pill: { text: "statement", tone: "neutral" }, overdue: false },
  ],
  receipts: [
    { key: "x1", merchant: "Blue Kettle Coffee", amount: "$6.50" },
    { key: "x2", merchant: "Doorstep Eats", amount: "$23.18" },
    { key: "x3", merchant: "Klubhaus Berlin", amount: "€25.00" },
  ],
};
