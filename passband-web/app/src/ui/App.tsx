import { useCallback, useEffect, useState } from "react";
import { Inbox } from "../app/inbox";
import { navigate, useRoute } from "../app/router";
import { client, transport } from "../app/services";
import { SitrepStore } from "../app/sitrep";
import { Connect } from "./Connect";
import { MailPage } from "./MailPage";
import { Reader } from "./Reader";
import { Shell } from "./Shell";
import { SignIn } from "./SignIn";
import { SitrepPage } from "./SitrepPage";
import { Toasts } from "./Toasts";

const inbox = new Inbox(client);
const sitrep = new SitrepStore(client);

export function App() {
  const route = useRoute();
  const [ready, setReady] = useState(false);
  const [paired, setPaired] = useState(false);

  useEffect(() => {
    void transport.ready().then(() => {
      setPaired(transport.server() !== null);
      setReady(true);
    });
  }, []);

  const signedOut = useCallback(() => setPaired(false), []);
  const onPaired = useCallback(() => {
    setPaired(true);
    navigate({ name: "home" }, true);
  }, []);

  if (route.name === "connect") return <Connect onPaired={onPaired} />;
  if (!ready) return <div className="splash" aria-busy="true" />;
  if (!paired) return <SignIn />;

  const where = route.name === "mail" ? "mail" : route.name === "thread" ? "mail" : "sitrep";
  return (
    <>
      <Shell route={route} where={where} onSignedOut={signedOut}>
        {route.name === "thread" ? (
          <Reader threadId={route.threadId} inbox={inbox} onSignedOut={signedOut} />
        ) : route.name === "mail" ? (
          <MailPage inbox={inbox} onSignedOut={signedOut} />
        ) : (
          <SitrepPage store={sitrep} onSignedOut={signedOut} />
        )}
      </Shell>
      <Toasts />
    </>
  );
}
