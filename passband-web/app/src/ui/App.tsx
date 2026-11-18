import { useCallback, useEffect, useState } from "react";
import { Inbox } from "../app/inbox";
import { navigate, useRoute } from "../app/router";
import { client, transport } from "../app/services";
import { SitrepStore } from "../app/sitrep";
import { WalkStore, current, useWalk } from "../app/walk";
import type { EyesItem } from "../sitrep/model";
import { Connect } from "./Connect";
import { nudgeLedger } from "./DesktopApp";
import { MailPage } from "./MailPage";
import { Reader, type ReaderWalk } from "./Reader";
import { Shell } from "./Shell";
import { SignIn } from "./SignIn";
import { SitrepPage } from "./SitrepPage";
import { Toasts } from "./Toasts";
import "./flow.css";

const inbox = new Inbox(client);
const sitrep = new SitrepStore(client);
const walks = new WalkStore();

/** Each step of a walk REPLACES the history entry, so Back leaves flow mode. */
const show = (item: EyesItem | undefined) =>
  item ? navigate({ name: "thread", threadId: item.threadId }, true) : navigate({ name: "home" }, true);

const flow = {
  start(eyes: EyesItem[], highlighted?: EyesItem) {
    const first = walks.start(eyes, highlighted);
    if (first) navigate({ name: "thread", threadId: first.threadId });
  },
  step(kind: "done" | "skip") {
    const w = walks.snapshot().walk;
    const item = w ? current(w) : undefined;
    if (!item) return;
    // The walk moves on at once; the done goes out behind it. Its undo walks
    // back onto the email when that is the one just behind.
    show(walks.step(kind));
    if (kind === "done") {
      void sitrep
        .done(item, () => {
          const at = walks.rewind("done", item.id);
          if (at) show(at);
        })
        .then((ok) => ok && nudgeLedger.action());
    }
  },
  move(by: 1 | -1) {
    show(walks.move(by));
  },
  end() {
    walks.end();
    navigate({ name: "home" }, true);
  },
};

export function App() {
  const route = useRoute();
  const w = useWalk(walks);
  const [ready, setReady] = useState(false);
  const [paired, setPaired] = useState(false);

  useEffect(() => {
    void transport.ready().then(() => {
      setPaired(transport.server() !== null);
      setReady(true);
    });
  }, []);

  // Leaving the reader any other way (the rail, Back) ends the walk; the tally
  // so far still counts.
  useEffect(() => {
    if (w.walk && route.name !== "thread") walks.end();
  }, [route.name, w.walk]);

  const signedOut = useCallback(() => setPaired(false), []);
  const onPaired = useCallback(() => {
    setPaired(true);
    navigate({ name: "home" }, true);
  }, []);

  if (route.name === "connect") return <Connect onPaired={onPaired} />;
  if (!ready) return <div className="splash" aria-busy="true" />;
  if (!paired) return <SignIn />;

  const walkItem = w.walk ? current(w.walk) : undefined;
  const readerWalk: ReaderWalk | undefined =
    w.walk && route.name === "thread" && walkItem?.threadId === route.threadId
      ? {
          position: w.walk.index + 1,
          total: w.walk.queue.length,
          done: w.walk.done,
          skipped: w.walk.skipped,
          step: flow.step,
          move: flow.move,
          end: flow.end,
        }
      : undefined;

  const where = route.name === "home" ? "sitrep" : readerWalk ? "flow" : "mail";
  return (
    <>
      <Shell route={route} where={where} onSignedOut={signedOut}>
        {route.name === "thread" ? (
          <Reader
            threadId={route.threadId}
            inbox={inbox}
            onSignedOut={signedOut}
            walk={readerWalk}
          />
        ) : route.name === "mail" ? (
          <MailPage inbox={inbox} onSignedOut={signedOut} />
        ) : (
          <SitrepPage
            store={sitrep}
            onSignedOut={signedOut}
            onFlow={flow.start}
            finished={w.finished}
            onDismissFinished={() => walks.dismissSummary()}
          />
        )}
      </Shell>
      <Toasts />
    </>
  );
}
