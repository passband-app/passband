// The static pages' analytics: /about, /privacy, /terms, /trust, /security and
// /self-host are plain HTML (crawlers and OAuth reviewers read them without
// running the React bundle), so they load this small entry instead of
// main.js. Same PostHog setup and the same cookie as the homepage, so a visit
// that goes home -> /trust -> /privacy is one session and one recording.
// Those pages are always the dark theme.
import { startAnalytics } from "./analytics";

startAnalytics("dark");
