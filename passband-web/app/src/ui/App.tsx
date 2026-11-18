import { useEffect, useState } from "react";
import { Inbox } from "../app/inbox";
import { navigate, useRoute } from "../app/router";
import { client, transport } from "../app/services";
import { Connect } from "./Connect";
import { Home } from "./Home";
import { Reader } from "./Reader";
import { SignIn } from "./SignIn";
import { Toasts } from "./Toasts";

const inbox = new Inbox(client);

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

  if (route.name === "connect") {
    return (
      <Connect
        onPaired={() => {
          setPaired(true);
          navigate({ name: "home" }, true);
        }}
      />
    );
  }
  if (!ready) return <div className="splash" aria-busy="true" />;
  if (!paired) return <SignIn />;

  const signedOut = () => setPaired(false);
  return (
    <>
      {route.name === "thread" ? (
        <Reader threadId={route.threadId} inbox={inbox} onSignedOut={signedOut} />
      ) : (
        <Home inbox={inbox} onSignedOut={signedOut} />
      )}
      <Toasts />
    </>
  );
}
