// THE LANDING PAGE'S ANALYTICS. The site is marketing, not the product, so
// it records as much as PostHog will take: every pageview and page leave,
// every click (autocapture), heatmaps, rage and dead clicks, scroll depth,
// web vitals, uncaught errors, full session recordings, UTM and referrer,
// and, once someone joins the waitlist, who they are. The app's own
// analytics (Lib/Analytics.swift, docs/ANALYTICS.md) are a different thing
// with a closed vocabulary; none of that applies here, and none of this
// applies there. /privacy and /trust say what this collects.
//
// Same PostHog project as the apps, so a visitor who joins the waitlist and
// later signs in is one person from first visit to first sitrep.
//
// Events go to /ingest on passband.app itself, which CloudFront forwards to
// PostHog (deploy/aws/tofu/main/site.tf). That keeps the site's CSP at
// connect-src 'self' and keeps ad blockers from dropping the events.
// Everything PostHog would otherwise load from its CDN (the recorder, web
// vitals, dead clicks, exceptions) is bundled into main.js instead, because
// script-src 'self' blocks the CDN.
//
// Only passband.app reports. Local servers, previews and ?theme=dark checks
// on localhost send nothing.

import posthog from "posthog-js/dist/module.no-external";
// posthog-recorder, not recorder: it carries both halves replay needs
// (rrweb and initSessionRecording). The plain recorder bundle has only rrweb,
// and replay then sits at "starting" forever, waiting on a CDN it may not use.
import "posthog-js/dist/posthog-recorder";
import "posthog-js/dist/web-vitals";
import "posthog-js/dist/dead-clicks-autocapture";
import "posthog-js/dist/exception-autocapture";

// Public by design: the same project key the Mac app compiles in.
const KEY = "phc_uuYkpwXQYxrz33f4bQsi5ozAQ6LL5kPsnrnU7vVvshni";
const LIVE = typeof location !== "undefined" && location.hostname === "passband.app";

/** Every custom event the site sends, so a dashboard has one list to read. */
export type SiteEvent =
  | "site_section_viewed" // { section }
  | "site_scroll_depth" // { percent: 25 | 50 | 75 | 90 | 100 }
  | "site_squelch_closed" // the hero's squelch reached full
  | "site_tour_step" // { tour: "sitrep" | "calendar", step, title }
  | "site_mail_landed" // { id }: an arriving email reached its zone
  | "site_cta_clicked" // { location: "hero" | "close" }
  | "site_waitlist_opened" // the form opened in place of the button
  | "site_waitlist_field_focused" // { field: "name" | "email" }
  | "site_waitlist_submitted"
  | "site_waitlist_joined"
  | "site_waitlist_failed"; // { status } (0 for a network error)

let started = false;

export function startAnalytics(theme: "light" | "dark") {
  if (!LIVE || started) return;
  started = true;
  posthog.init(KEY, {
    api_host: "/ingest",
    ui_host: "https://us.posthog.com",
    // Nothing from a CDN: the extensions are bundled above.
    disable_external_dependency_loading: true,
    // Everyone gets a person profile and a cookie, from the first visit.
    person_profiles: "always",
    persistence: "localStorage+cookie",
    cross_subdomain_cookie: true,
    capture_pageview: "history_change",
    capture_pageleave: true,
    autocapture: true,
    rageclick: true,
    capture_dead_clicks: true,
    enable_heatmaps: true,
    capture_performance: { web_vitals: true, network_timing: true },
    capture_exceptions: true,
    disable_session_recording: false,
    session_recording: {
      // The waitlist form is the only input on the page, and joining
      // identifies the visitor by that same address anyway.
      maskAllInputs: false,
      recordCrossOriginIframes: false,
    },
    save_referrer: true,
    store_google: true,
  });
  posthog.register({ site: "passband.app", theme });
}

export function track(event: SiteEvent, props?: Record<string, unknown>) {
  if (!started) return;
  posthog.capture(event, props);
}

/** The visitor told us who they are. Ties every earlier event to them. */
export function identify(email: string, name: string) {
  if (!started) return;
  posthog.identify(email.trim().toLowerCase(), { email: email.trim(), name: name.trim() || undefined });
}
