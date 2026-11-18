// Design harness: the real Shell and SitrepView over synthetic data, so the
// layout can be looked at (and screenshotted) without a daemon. e2e builds only.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { SitrepView } from "../src/ui/SitrepView";
import { Shell } from "../src/ui/Shell";
import { SAMPLE } from "./sitrep-sample";

const params = new URLSearchParams(location.search);
const empty = params.has("empty");

function Harness() {
  const [sel, setSel] = useState(-1);
  const sitrep = empty
    ? { ...SAMPLE, headline: null, eyes: [], reading: [], calendar: [], shipments: [], billing: [], receipts: [] }
    : SAMPLE;
  return (
    <Shell route={{ name: "home" }} where="sitrep" account="sam@example.com" onSignedOut={() => {}}>
      <SitrepView
        sitrep={sitrep}
        selected={sel}
        onSelect={setSel}
        handlers={{ open: () => {}, openSealed: () => {}, done: () => {}, archive: () => {} }}
      />
    </Shell>
  );
}

createRoot(document.getElementById("root")!).render(<Harness />);
