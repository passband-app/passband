// Search harness: the real SearchPage against a real daemon, with a device
// credential written to this origin's store before the app's transport loads.
// e2e builds only. `?daemon=<origin>&token=<bearer>`.
import { createRoot } from "react-dom/client";
import { IdbCredentialStore } from "../src/session/idb";

const params = new URLSearchParams(location.search);
const server = params.get("daemon") ?? "http://127.0.0.1:8855";
const token = params.get("token") ?? "spike-token";

await new IdbCredentialStore().save({ server, token });
// Imported only now, so the transport reads the credential just saved.
const { SearchPage } = await import("../src/ui/SearchPage");
const opened = document.getElementById("opened")!;

createRoot(document.getElementById("root")!).render(
  <SearchPage
    initialQuery={params.get("q") ?? undefined}
    onOpen={(threadId) => {
      opened.textContent = threadId;
    }}
    onSignedOut={() => {
      opened.textContent = "signed-out";
    }}
  />,
);
