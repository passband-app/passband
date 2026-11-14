import { createRoot } from "react-dom/client";
import { App, THEME } from "./App";

// The theme goes on <html> from here, not from an inline script in
// index.html: the site's CSP is script-src 'self', which blocks inline
// scripts, so one there never runs in production (it does locally, where
// nothing sends the header).
document.documentElement.dataset.theme = THEME;

// ONE PAGE, one bundle, no router dep. /waitlist is a state of App rather than
// a document of its own, so the path is read there (and written back with
// pushState) instead of picking a component here.
createRoot(document.getElementById("root")!).render(<App />);
