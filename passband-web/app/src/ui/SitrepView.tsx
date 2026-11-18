// The sitrep, drawn. Presentational only: it renders a `Sitrep` view model
// (src/sitrep/model.ts) and reports what was clicked.
//
// Layout follows the Mac's Views/SitrepView.swift: a greeting and one serif
// headline, then "For your eyes" and "Reading" on the left, and the Calendar,
// Shipments, Billing and Receipts cards down the right. Every card keeps its
// empty state, so the right-hand column never disappears.
import type { ReactNode } from "react";
import type { EyesItem, Pill, Sitrep } from "../sitrep/model";
import {
  ArchiveIcon,
  BankIcon,
  BoxIcon,
  CalendarIcon,
  CheckIcon,
  EyeIcon,
  PaperclipIcon,
  ReadingIcon,
  ReceiptIcon,
} from "./icons";

export interface SitrepHandlers {
  open: (threadId: string) => void;
  done: (item: EyesItem) => void;
  archive: (item: EyesItem) => void;
}

export function SitrepView({
  sitrep,
  selected,
  onSelect,
  handlers,
  banner,
}: {
  sitrep: Sitrep;
  /** Index into `sitrep.eyes` of the keyboard cursor, or -1. */
  selected: number;
  onSelect: (i: number) => void;
  handlers: SitrepHandlers;
  banner?: ReactNode;
}) {
  return (
    <div className="sitrep">
      <section className="hero">
        <div className="greeting">{sitrep.greeting}</div>
        {/* Dim only while the daemon's sentence is on its way over a board with
            mail on it; the empty board's line is final. */}
        <h1 className={`headline${!sitrep.headline && sitrep.eyes.length > 0 ? " pending" : ""}`}>
          {sitrep.headline ?? fallbackHeadline(sitrep.eyes.length)}
        </h1>
      </section>
      {banner}
      <div className="board">
        <div className="col">
          <Zone icon={<EyeIcon />} title="For your eyes" count={sitrep.eyes.length}>
            {sitrep.eyes.length === 0 ? (
              <p className="zone-empty">Nothing needs you right now.</p>
            ) : (
              <ul className="eyes">
                {sitrep.eyes.map((e, i) => (
                  <EyeRow
                    key={e.id}
                    item={e}
                    selected={i === selected}
                    onHover={() => onSelect(i)}
                    handlers={handlers}
                  />
                ))}
              </ul>
            )}
          </Zone>
          <Zone
            icon={<ReadingIcon />}
            title="Reading"
            count={sitrep.reading.length}
            sub="newsletters, announcements, and offers"
          >
            {sitrep.reading.length === 0 ? (
              <p className="zone-empty">Nothing set aside to read.</p>
            ) : (
              <div className="tiles">
                {sitrep.reading.map((g) => (
                  <button key={g.key} className="tile" onClick={() => handlers.open(g.threadId)}>
                    <span className={`logo avatar h${g.avatarHue}`} aria-hidden="true">
                      {g.initials}
                    </span>
                    <span style={{ minWidth: 0 }}>
                      <span className="top">
                        <span className="name">{g.sender}</span>
                        <span className="n">
                          {g.count} {g.count === 1 ? "email" : "emails"}
                        </span>
                      </span>
                      <span className="snip">{g.snippet}</span>
                    </span>
                  </button>
                ))}
              </div>
            )}
          </Zone>
        </div>
        <div className="col">
          <Zone icon={<CalendarIcon />} title="Calendar" count={sitrep.calendar.length} tint="accent">
            {sitrep.calendar.length === 0 ? (
              <p className="zone-empty">Nothing coming up.</p>
            ) : (
              <ul className="rows">
                {sitrep.calendar.map((c) => (
                  <Row key={c.key} threadId={c.threadId} open={handlers.open}>
                    <span className="t">{c.title}</span>
                    {c.pill && <PillView pill={c.pill} />}
                    <span className="when">{c.when}</span>
                  </Row>
                ))}
              </ul>
            )}
          </Zone>
          <Zone icon={<BoxIcon />} title="Shipments" count={sitrep.shipments.length} tint="warn">
            {sitrep.shipments.length === 0 ? (
              <p className="zone-empty">No packages on the way.</p>
            ) : (
              sitrep.shipments.map((s) => (
                <div
                  key={s.key}
                  className="ship"
                  role={s.threadId ? "button" : undefined}
                  tabIndex={s.threadId ? 0 : undefined}
                  onClick={() => s.threadId && handlers.open(s.threadId)}
                >
                  <span className="fav" aria-hidden="true">
                    <BoxIcon size={15} />
                  </span>
                  <span style={{ minWidth: 0 }}>
                    <div className="t">{s.title}</div>
                    <div className="d">{s.detail}</div>
                  </span>
                  <PillView pill={s.status} />
                  {s.eta && <span className="eta pill neutral">{s.eta}</span>}
                </div>
              ))
            )}
          </Zone>
          <Zone icon={<BankIcon />} title="Billing" count={sitrep.billing.length} tint="accent">
            {sitrep.billing.length === 0 ? (
              <p className="zone-empty">No bills due.</p>
            ) : (
              <ul className="rows">
                {sitrep.billing.map((b) => (
                  <Row key={b.key} threadId={b.threadId} open={handlers.open}>
                    <span className="t">{b.payee}</span>
                    {b.pill && <PillView pill={b.pill} />}
                    {b.amount && <span className={`amt${b.overdue ? " overdue" : ""}`}>{b.amount}</span>}
                  </Row>
                ))}
              </ul>
            )}
          </Zone>
          <Zone icon={<ReceiptIcon />} title="Receipts" count={sitrep.receipts.length} tint="positive">
            {sitrep.receipts.length === 0 ? (
              <p className="zone-empty">No recent receipts.</p>
            ) : (
              <ul className="rows">
                {sitrep.receipts.map((r) => (
                  <Row key={r.key} threadId={r.threadId} open={handlers.open}>
                    <span className="t">{r.merchant}</span>
                    <span className="amt">{r.amount}</span>
                  </Row>
                ))}
              </ul>
            )}
          </Zone>
        </div>
      </div>
    </div>
  );
}

/** What the hero says before (or without) the daemon's sentence. */
export function fallbackHeadline(n: number): string {
  if (n === 0) return "You're all clear.";
  const words = ["Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten"];
  const spelled = words[n] ?? String(n);
  return `${spelled} ${n === 1 ? "thing needs" : "things need"} you today.`;
}

function Zone({
  icon,
  title,
  count,
  sub,
  tint,
  children,
}: {
  icon: ReactNode;
  title: string;
  count: number;
  sub?: string;
  tint?: "accent" | "warn" | "positive";
  children: ReactNode;
}) {
  return (
    <section className={`zone glass${tint ? ` tint-${tint}` : ""}`} aria-label={title}>
      <header className="zone-head">
        <span className="ico">{icon}</span>
        <h2>{title}</h2>
        {count > 0 && <span className="count">{count}</span>}
        {sub && <span className="sub">{sub}</span>}
      </header>
      {children}
    </section>
  );
}

function EyeRow({
  item,
  selected,
  onHover,
  handlers,
}: {
  item: EyesItem;
  selected: boolean;
  onHover: () => void;
  handlers: SitrepHandlers;
}) {
  return (
    <li
      className={`eye${selected ? " selected" : ""}${item.overdue ? " overdue" : ""}`}
      onMouseEnter={onHover}
      onClick={() => handlers.open(item.threadId)}
    >
      <span className={`avatar h${item.avatarHue}`} aria-hidden="true">
        {item.initials}
      </span>
      <span className="who">
        {item.sender}
        {item.hasAttachment && <PaperclipIcon size={13} label="has attachments" />}
      </span>
      <span className="line">{item.line}</span>
      <span className="end">
        {item.pill && <PillView pill={item.pill} />}
        <span className="verbs">
          <button
            className="icon-btn"
            title="Done (e)"
            aria-label="Done"
            onClick={(ev) => {
              ev.stopPropagation();
              handlers.done(item);
            }}
          >
            <CheckIcon size={16} />
          </button>
          <button
            className="icon-btn"
            title="Archive"
            aria-label="Archive"
            onClick={(ev) => {
              ev.stopPropagation();
              handlers.archive(item);
            }}
          >
            <ArchiveIcon size={16} />
          </button>
        </span>
      </span>
    </li>
  );
}

function Row({
  threadId,
  open,
  children,
}: {
  threadId?: string;
  open: (threadId: string) => void;
  children: ReactNode;
}) {
  return (
    <li
      className={`r${threadId ? " clickable" : ""}`}
      onClick={threadId ? () => open(threadId) : undefined}
    >
      {children}
    </li>
  );
}

export function PillView({ pill }: { pill: Pill }) {
  return <span className={`pill ${pill.tone}`}>{pill.text}</span>;
}
